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
use psx_engine::SkyDirectionProjector;
#[cfg(feature = "vis-full-active-chunks")]
use psx_engine::draw_indexed_cached_room_vertex_lit_all_cells;
#[cfg(feature = "cd-stream-bench")]
use psx_engine::CompactCollisionRoom;
#[cfg(feature = "world-grid-visible")]
use psx_engine::GridVisibilityStats;
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
use psx_engine::GridVisibleCell;
use psx_engine::{
    apply_model_pose_translation, button, compute_joint_world_transform, telemetry, Angle, App,
    CachedRoomCell, CachedRoomDepthMode, CachedRoomSubdivisionMode, CachedRoomSurface,
    CharacterCollision, CharacterCollisionAabb, CharacterCollisionCylinder, CharacterCollisionRoom,
    CharacterMotorAnim, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, Config,
    Ctx, CullMode, DepthBand, DepthPolicy, DepthRange, JointViewTransform, JointWorldTransform,
    LoadedWorldCameraGte, LocalToWorldScale, Mat3I16, MaterialTint, ModelPoseTranslation, OtFrame,
    PointLightSample, PrimitivePacketArena, PrimitivePacketScratch, PrimitiveSink, ProjectedVertex,
    Rgb8, RoomPoint, RoomRender, RuntimeCollisionRoom, RuntimeRoom, Scene, SchedulerConfig,
    SimTick, TexturedModelGeometry, TexturedModelRenderFace, TexturedModelRenderStats,
    ThirdPersonCameraConfig, ThirdPersonCameraInput, ThirdPersonCameraState,
    ThirdPersonCameraTarget, VideoHz, VisualPacing, WorldCamera, WorldProjection,
    WorldRenderMaterial, WorldRenderPass, WorldSurfaceLighting, WorldSurfaceOptions,
    WorldSurfaceSample, WorldTriCommand, WorldVertex, Q12, Q8,
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
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{
    draw_line_mono, draw_tri_flat_blended,
    material::{BlendMode, TextureMaterial, TextureWindow},
    ot::OrderingTable,
    prim::{QuadTexturedMaterial, TriTextured, TriTexturedGouraud},
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
    LevelBoxPropRecord, LevelCameraRecord, LevelCharacterRecord, LevelChunkRecord,
    LevelFarVistaRecord, LevelImagePropRecord, LevelMaterialRecord, LevelMaterialSidedness,
    LevelModelFrameBoundsRecord, LevelModelRecord, LevelModelSocketRecord, LevelRoomRecord,
    LevelSkyRecord, ModelClipIndex, ModelClipTableIndex, ModelIndex, ModelSocketIndex,
    OptionalModelClipIndex, ParticleEmitterRecord, ResidencyManager, RoomIndex, RuntimeDebugMask,
    WeaponHitShapeRecord, CHARACTER_ANIMATION_ACTION_COUNT,
};
#[cfg(feature = "cd-stream-bench")]
use psx_level::{
    streamed_room_chunk_header, LevelCachedRoomCellRecord, LevelCachedRoomSurfaceRecord,
    LevelCachedRoomVertexRecord, STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT,
    STREAMED_ROOM_CHUNK_HEADER_BYTES, STREAMED_ROOM_CHUNK_MAGIC, STREAMED_ROOM_CHUNK_VERSION,
};
use psx_vram::{upload_bytes, Clut, TexDepth, TextureWindowAtlas, Tpage, VramRect};

#[cfg(feature = "cd-stream-bench")]
mod cd_stream;
mod input;
mod overlay;
mod runtime_schedule;
mod vram_upload;

use input::*;
use overlay::*;
use runtime_schedule::RUNTIME_SCHEDULE;
use vram_upload::*;

// Placeholder manifests reference unused statics; populated
// manifests reference all of them. Quiet either side here.
#[allow(dead_code, unused_imports)]
mod generated {
    include!(env!("PSXED_PLAYTEST_MANIFEST"));
}

use generated::{
    ASSETS, BOX_PROPS, CACHED_ROOM_DEPTH_MODE, CACHED_ROOM_DRAW_ORDER_MODE,
    CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE, CACHED_ROOM_TEXTURE_SPLIT_MODE, CHARACTERS, ENTITIES,
    EQUIPMENT, IMAGE_PROPS, LIGHTS, MATERIALS, MODELS, MODEL_CLIPS, MODEL_CLIP_BOUNDS,
    MODEL_FRAME_BOUNDS, MODEL_INSTANCES, MODEL_SOCKETS, PARTICLE_EMITTERS, PLAYER_CONTROLLER,
    PLAYER_SPAWN, ROOMS, ROOM_CACHE_CELLS, ROOM_CACHE_CELL_VERTICES, ROOM_CACHE_SURFACES,
    ROOM_CACHE_VERTICES, ROOM_CHUNKS, ROOM_PORTALS, ROOM_RESIDENCY, ROOM_SURFACE_CACHES,
    ROOM_VISIBILITY, UI_NODES, VISIBILITY_CELLS, WEAPONS, WEAPON_HITBOXES,
};
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
/// CLUT strip used by room material textures. Keep it outside the
/// 320-pixel-wide double-buffered framebuffer (`x=0..319`,
/// `y=0..479`) so frame clears cannot overwrite palettes.
const ROOM_CLUT_BASE_X: u16 = 320;
const ROOM_CLUT_STRIDE: u16 = 16;
const ROOM_CLUT_Y: u16 = 480;

const MODEL_TPAGE: Tpage = Tpage::new(384, 256, TexDepth::Bit8);
/// Minimum allocation quantum for an 8bpp model atlas. The GPU
/// texture-page X field is 64-halfword aligned; keeping every atlas
/// on that boundary lets meshes use local UVs unchanged.
const MODEL_TPAGE_SLOT_HALFWORDS: u16 = 64;
/// Maximum halfword width addressable by one 8bpp texture page.
const MODEL_TPAGE_MAX_HALFWORDS: u16 = 128;
/// First CLUT row used by model atlases. 256-entry CLUTs span
/// a single row; we step one row down per uploaded atlas, so
/// `MODEL_CLUT_BASE_Y + n` is the row for the n-th atlas.
const MODEL_CLUT_BASE_Y: u16 = 484;

/// Cooked sky panoramas occupy two side-by-side 4bpp pages. The
/// texture pixels are outside the double-buffered framebuffer and
/// model-atlas upload regions; each horizontal band gets a dedicated
/// CLUT row so the sky can spend 16 colours per altitude range.
const SKY_PANORAMA_LEFT_TPAGE: Tpage = Tpage::new(896, 256, TexDepth::Bit4);
const SKY_PANORAMA_RIGHT_TPAGE: Tpage = Tpage::new(960, 256, TexDepth::Bit4);
const SKY_PANORAMA_CLUT_X: u16 = 320;
const SKY_PANORAMA_CLUT_Y: u16 = 481;
const SKY_PANORAMA_CLUT_ENTRIES: u16 = 16;
const SKY_PANORAMA_PALETTE_BANDS: usize = 8;
const SKY_PANORAMA_WIDTH: u16 = 512;
const SKY_PANORAMA_HEIGHT: u16 = 256;
const SKY_PANORAMA_PAGE_WIDTH: u16 = 256;
const SKY_CYCLORAMA_GRID_POINTS_MAX: usize =
    (SKY_CYCLORAMA_COLUMNS_MAX as usize + 1) * (SKY_PANORAMA_PALETTE_BANDS + 1);
/// Model atlases pack left-to-right until the reserved sky page.
const MODEL_TPAGE_LIMIT_X: u16 = SKY_PANORAMA_LEFT_TPAGE.x();
const SKY_CYCLORAMA_COLUMNS_MIN: u8 = 8;
const SKY_CYCLORAMA_COLUMNS_MAX: u8 = 12;

/// 4bpp 8x8 BIOS-style font atlas for the analog-mode gate prompt.
const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const TARGET_LOCK_OUTER: i32 = 25;
const TARGET_LOCK_INNER: i32 = 13;
const TARGET_LOCK_TRI_HALF_WIDTH: i32 = 8;
const TARGET_LOCK_RED: (u8, u8, u8) = (225, 18, 24);
const TARGET_LOCK_ROTATION_FRAMES: u32 = 360;
static SHADOW_CIRCLE_BLOB: &[u8] = include_bytes!("../assets/shadow_circle_64.psxt");
const SHADOW_TPAGE: Tpage = Tpage::new(576, 0, TexDepth::Bit4);
const SHADOW_TEXEL_U: u8 = 64;
const SHADOW_TEXTURE_X: u16 = SHADOW_TPAGE.x() + ((SHADOW_TEXEL_U as u16) / 4);
const SHADOW_CLUT: Clut = Clut::new(336, 481);
const SHADOW_UV_MAX: u8 = SHADOW_TEXEL_U + 63;
const PARTICLE_TPAGE: Tpage = SHADOW_TPAGE;
const PARTICLE_TEXEL_U: u8 = 0;
const PARTICLE_TEXEL_V: u8 = 0;
const PARTICLE_TEXTURE_SIZE: u16 = 16;
const PARTICLE_UV_MAX: u8 = PARTICLE_TEXEL_U + PARTICLE_TEXTURE_SIZE as u8 - 1;
const PARTICLE_TEXTURE_X: u16 = PARTICLE_TPAGE.x() + ((PARTICLE_TEXEL_U as u16) / 4);
const PARTICLE_TEXTURE_HALFWORDS_PER_ROW: u16 = PARTICLE_TEXTURE_SIZE / 4;
const PARTICLE_CLUT: Clut = Clut::new(352, 481);

const SCREEN_W: i16 = 320;
const SCREEN_H: i16 = 240;
const SCREEN_CX: i16 = 160;
const SCREEN_CY: i16 = 120;
const ATMOSPHERE_PARTICLE_MAX: u32 = 96;
const ATMOSPHERE_SCREEN_MARGIN: i32 = 24;
const ATMOSPHERE_WRAP_W: i32 = SCREEN_W as i32 + ATMOSPHERE_SCREEN_MARGIN * 2;
const ATMOSPHERE_WRAP_H: i32 = SCREEN_H as i32 + ATMOSPHERE_SCREEN_MARGIN * 2;
const PARTICLE_EMITTER_DRAW_CAP: u16 = 64;
const PARTICLE_MIN_SCREEN_SIZE: i16 = 2;
const PARTICLE_MAX_SCREEN_SIZE: i16 = 18;
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
const IMAGE_PROP_DEPTH_BIAS: i32 = 256;
const COLLISION_DEBUG_BUTTON: u16 = button::L3;
const COLLISION_DEBUG_SEGMENTS: usize = 8;
const COLLISION_DEBUG_FLOOR_LIFT: i32 = 8;
const FLOOR_LINK_CROSS_EPSILON: i32 = 32;
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

fn room_index_debug_mask(index: RoomIndex) -> RuntimeDebugMask {
    RuntimeDebugMask::from_room(index)
}

fn emit_room_chunk_mask(counter_lo: u16, counter_hi: u16, mask: RuntimeDebugMask) {
    telemetry::counter(counter_lo, mask.lo());
    telemetry::counter(counter_hi, mask.hi());
}

const DEBUG_LOG_LINE_CAP: usize = 256;
/// Master gate for the verbose portal-visibility snapshot log. Default off: the
/// snapshot emits many lines one byte at a time via `write_volatile` to the
/// trapped emulator log port, and every trapped byte costs the emulator
/// thousands of cycles, so a single snapshot smears ~1M guest cycles onto its
/// tick and reads as a frametime spike. Its `should_debug_log_*` predicate is
/// almost always true (some portal is always rejected), so it fired on a fixed
/// cooldown in normal runs. Keep false for play/perf; flip to true only when
/// debugging portal traversal.
const PORTAL_VIS_DEBUG_LOGS: bool = false;
const PORTAL_VIS_DEBUG_LOG_COOLDOWN_TICKS: u8 = 120;
const PORTAL_VIS_DEBUG_VERBOSE_CLIPS: bool = false;
const PORTAL_VIS_DEBUG_LOG_MAX_FRUSTUMS: usize = 4;
const PORTAL_VIS_DEBUG_LOG_MAX_PORTALS: usize = 16;
const POST_CROSS_RENDER_DEBUG_LOGS: bool = false;

struct DebugLogLine {
    bytes: [u8; DEBUG_LOG_LINE_CAP],
    len: usize,
}

impl DebugLogLine {
    fn new(prefix: &str) -> Self {
        let mut line = Self {
            bytes: [0; DEBUG_LOG_LINE_CAP],
            len: 0,
        };
        line.push_str(prefix);
        line
    }

    fn push_str(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            self.push_byte(byte);
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len < self.bytes.len() {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }

    fn push_u32(&mut self, value: u32) {
        let mut scratch = [0u8; 10];
        let mut remaining = value;
        let mut len = 0usize;
        loop {
            scratch[len] = b'0' + (remaining % 10) as u8;
            len += 1;
            remaining /= 10;
            if remaining == 0 {
                break;
            }
        }
        while len > 0 {
            len -= 1;
            self.push_byte(scratch[len]);
        }
    }

    fn push_i32(&mut self, value: i32) {
        if value < 0 {
            self.push_byte(b'-');
            self.push_u32(value.wrapping_neg() as u32);
        } else {
            self.push_u32(value as u32);
        }
    }

    fn push_room(&mut self, room: RoomIndex) {
        self.push_u32(room.raw() as u32);
    }

    fn push_bool(&mut self, value: bool) {
        self.push_byte(if value { b'1' } else { b'0' });
    }

    fn push_point(&mut self, point: RoomPoint) {
        self.push_byte(b'(');
        self.push_i32(point.x);
        self.push_byte(b',');
        self.push_i32(point.y);
        self.push_byte(b',');
        self.push_i32(point.z);
        self.push_byte(b')');
    }

    fn push_hex_u32_digits(&mut self, value: u32, pad_to_eight: bool) {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        if value == 0 && !pad_to_eight {
            self.push_byte(b'0');
            return;
        }
        let mut started = false;
        let mut shift = 28i32;
        while shift >= 0 {
            let nibble = ((value >> shift) & 0xF) as usize;
            if nibble != 0 || started || pad_to_eight || shift == 0 {
                started = true;
                self.push_byte(DIGITS[nibble]);
            }
            shift -= 4;
        }
    }

    fn push_hex_mask(&mut self, mask: RuntimeDebugMask) {
        self.push_str("0x");
        if mask.hi() != 0 {
            self.push_hex_u32_digits(mask.hi(), false);
            self.push_hex_u32_digits(mask.lo(), true);
        } else {
            self.push_hex_u32_digits(mask.lo(), false);
        }
    }

    fn emit(&self) {
        telemetry::debug_line(&self.bytes[..self.len]);
    }
}

fn debug_log_room_transition(
    previous_room: RoomIndex,
    next_room: RoomIndex,
    previous_local: RoomPoint,
    next_local: RoomPoint,
    global: RoomPoint,
    camera_before: RoomPoint,
    camera_after: RoomPoint,
) {
    if !POST_CROSS_RENDER_DEBUG_LOGS {
        return;
    }
    let mut line = DebugLogLine::new("room cross prev=");
    line.push_room(previous_room);
    line.push_str(" next=");
    line.push_room(next_room);
    line.push_str(" player_local=");
    line.push_point(previous_local);
    line.push_str(" -> ");
    line.push_point(next_local);
    line.push_str(" global=");
    line.push_point(global);
    line.push_str(" camera=");
    line.push_point(camera_before);
    line.push_str(" -> ");
    line.push_point(camera_after);
    line.emit();
}

fn debug_log_room_window_after_cross(
    room: RoomIndex,
    visible_count: usize,
    frontier_count: usize,
    visible_mask: RuntimeDebugMask,
    active_mask: RuntimeDebugMask,
    drawable_mask: RuntimeDebugMask,
    loading_mask: RuntimeDebugMask,
    missing_mask: RuntimeDebugMask,
    build_failed_mask: RuntimeDebugMask,
    current_render_ready: bool,
    current_collision_ready: bool,
    portals_tested: u16,
    portals_accepted: u16,
) {
    if !POST_CROSS_RENDER_DEBUG_LOGS {
        return;
    }
    let mut line = DebugLogLine::new("room window room=");
    line.push_room(room);
    line.push_str(" visible=");
    line.push_u32(visible_count.min(u32::MAX as usize) as u32);
    line.push_str(" frontier=");
    line.push_u32(frontier_count.min(u32::MAX as usize) as u32);
    line.push_str(" tested=");
    line.push_u32(portals_tested as u32);
    line.push_str(" accepted=");
    line.push_u32(portals_accepted as u32);
    line.push_str(" vis=");
    line.push_hex_mask(visible_mask);
    line.push_str(" active=");
    line.push_hex_mask(active_mask);
    line.push_str(" draw=");
    line.push_hex_mask(drawable_mask);
    line.push_str(" loading=");
    line.push_hex_mask(loading_mask);
    line.push_str(" missing=");
    line.push_hex_mask(missing_mask);
    line.push_str(" build_fail=");
    line.push_hex_mask(build_failed_mask);
    line.push_str(" render=");
    line.push_bool(current_render_ready);
    line.push_str(" coll=");
    line.push_bool(current_collision_ready);
    line.emit();
}

fn portal_debug_mask_bit(index: usize) -> RuntimeDebugMask {
    RuntimeDebugMask::from_index(index)
}

fn portal_debug_decision_name(decision: PortalClipDebugDecision) -> &'static str {
    match decision {
        PortalClipDebugDecision::Accepted => "accepted",
        PortalClipDebugDecision::Backface => "backface",
        PortalClipDebugDecision::EmptyProjection => "empty",
        PortalClipDebugDecision::NoWindowOverlap => "no_window",
        PortalClipDebugDecision::Tiny => "tiny",
    }
}

fn portal_debug_plane_name(plane: PortalClipDebugPlane) -> &'static str {
    match plane {
        PortalClipDebugPlane::None => "none",
        PortalClipDebugPlane::Near => "near",
        PortalClipDebugPlane::Left => "left",
        PortalClipDebugPlane::Right => "right",
        PortalClipDebugPlane::Bottom => "bottom",
        PortalClipDebugPlane::Top => "top",
    }
}

fn push_portal_debug_rect(line: &mut DebugLogLine, rect: PortalClipDebugRect) {
    line.push_byte(b'[');
    line.push_i32(rect.left_tan_q12);
    line.push_byte(b',');
    line.push_i32(rect.right_tan_q12);
    line.push_byte(b',');
    line.push_i32(rect.min_y_tan_q12);
    line.push_byte(b',');
    line.push_i32(rect.max_y_tan_q12);
    line.push_byte(b']');
}

fn push_optional_portal_debug_rect(line: &mut DebugLogLine, rect: Option<PortalClipDebugRect>) {
    if let Some(rect) = rect {
        push_portal_debug_rect(line, rect);
    } else {
        line.push_byte(b'-');
    }
}

fn portal_debug_center(portal: psx_level::LevelRoomPortalRecord) -> RoomPoint {
    RoomPoint::new(
        (portal.vertex_x[0]
            .saturating_add(portal.vertex_x[1])
            .saturating_add(portal.vertex_x[2])
            .saturating_add(portal.vertex_x[3]))
            / 4,
        (portal.vertex_y[0]
            .saturating_add(portal.vertex_y[1])
            .saturating_add(portal.vertex_y[2])
            .saturating_add(portal.vertex_y[3]))
            / 4,
        (portal.vertex_z[0]
            .saturating_add(portal.vertex_z[1])
            .saturating_add(portal.vertex_z[2])
            .saturating_add(portal.vertex_z[3]))
            / 4,
    )
}

fn portal_debug_view_center(clip: PortalClipDebug) -> RoomPoint {
    let mut x = 0i32;
    let mut y = 0i32;
    let mut z = 0i32;
    let mut i = 0usize;
    while i < 4 {
        let vertex = clip.view_vertices[i];
        x = x.saturating_add(vertex.x);
        y = y.saturating_add(vertex.y);
        z = z.saturating_add(vertex.z);
        i += 1;
    }
    RoomPoint::new(x / 4, y / 4, z / 4)
}

fn debug_log_portal_visibility_summary(
    current_room: RoomIndex,
    player_room: RoomIndex,
    player_local: RoomPoint,
    player_global: RoomPoint,
    view: ActiveRoomView,
    camera: PortalVisibilityCamera,
    result: &RuntimePortalVisibility,
) {
    let mut line = DebugLogLine::new("portal vis pose room=");
    line.push_room(current_room);
    line.push_str(" player_room=");
    line.push_room(player_room);
    line.push_str(" player_local=");
    line.push_point(player_local);
    line.push_str(" player_global=");
    line.push_point(player_global);
    line.emit();

    let stats = result.stats;
    let mut line = DebugLogLine::new("portal vis camera local=");
    line.push_point(view.position);
    line.push_str(" global=");
    line.push_point(RoomPoint::new(camera.x, camera.y, camera.z));
    line.push_str(" sy/cy/sp/cp=(");
    line.push_i32(camera.sin_yaw_q12);
    line.push_byte(b',');
    line.push_i32(camera.cos_yaw_q12);
    line.push_byte(b',');
    line.push_i32(camera.sin_pitch_q12);
    line.push_byte(b',');
    line.push_i32(camera.cos_pitch_q12);
    line.push_str(") near/far=");
    line.push_i32(camera.near_z);
    line.push_byte(b'/');
    line.push_i32(camera.far_z);
    line.push_str(" fov=");
    line.push_i32(camera.half_fov_x_tan_q12);
    line.push_byte(b'/');
    line.push_i32(camera.half_fov_y_tan_q12);
    line.emit();

    let mut line = DebugLogLine::new("portal vis stats rooms/fr=");
    line.push_u32(result.room_count.min(u32::MAX as usize) as u32);
    line.push_byte(b'/');
    line.push_u32(result.frustum_count.min(u32::MAX as usize) as u32);
    line.push_str(" test/acc=");
    line.push_u32(stats.portals_tested as u32);
    line.push_byte(b'/');
    line.push_u32(stats.portals_accepted as u32);
    line.push_str(" rej b/f/t=");
    line.push_u32(stats.reject_backface as u32);
    line.push_byte(b'/');
    line.push_u32(stats.reject_frustum as u32);
    line.push_byte(b'/');
    line.push_u32(stats.reject_tiny as u32);
    line.push_str(" cap r/f/d=");
    line.push_u32(stats.cap_room as u32);
    line.push_byte(b'/');
    line.push_u32(stats.cap_frustum as u32);
    line.push_byte(b'/');
    line.push_u32(stats.cap_depth as u32);
    line.emit();

    let mut line = DebugLogLine::new("portal vis masks visible=");
    line.push_hex_mask(result.visible_room_mask());
    line.push_str(" tested=");
    line.push_hex_mask(stats.tested_room_mask);
    line.push_str(" accepted=");
    line.push_hex_mask(stats.accepted_room_mask);
    line.push_str(" rej_rooms=");
    line.push_hex_mask(stats.reject_frustum_room_mask);
    line.push_str(" rej_portals=");
    line.push_hex_mask(stats.reject_frustum_portal_mask);
    line.emit();
}

fn debug_log_portal_clip_summary_line(
    portal_index: usize,
    portal: psx_level::LevelRoomPortalRecord,
    parent: PortalFrustum,
    clip: PortalClipDebug,
    stats: psx_level::portal_visibility::PortalVisibilityStats,
) {
    let portal_bit = portal_debug_mask_bit(portal_index);
    let tested = !portal_bit.is_empty() && stats.tested_portal_mask.contains_index(portal_index);
    let accepted =
        !portal_bit.is_empty() && stats.accepted_portal_mask.contains_index(portal_index);
    let rejected = !portal_bit.is_empty()
        && stats
            .reject_frustum_portal_mask
            .contains_index(portal_index);

    let mut line = DebugLogLine::new("portal p summary idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" src=");
    line.push_room(portal.source_room);
    line.push_str(" dst=");
    line.push_room(portal.destination_room);
    line.push_str(" depth=");
    line.push_u32(parent.depth as u32);
    line.push_str(" decision=");
    line.push_str(portal_debug_decision_name(clip.decision));
    line.push_str(" empty=");
    line.push_str(portal_debug_plane_name(clip.first_empty_plane));
    line.push_str(" t/a/r=");
    line.push_bool(tested);
    line.push_byte(b'/');
    line.push_bool(accepted);
    line.push_byte(b'/');
    line.push_bool(rejected);
    line.push_str(" world=");
    line.push_point(portal_debug_center(portal));
    line.emit();

    let mut line = DebugLogLine::new("portal p view idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" center=");
    line.push_point(portal_debug_view_center(clip));
    line.push_str(" parent=");
    push_portal_debug_rect(&mut line, clip.parent);
    line.push_str(" proj=");
    push_optional_portal_debug_rect(&mut line, clip.projected_bounds);
    line.push_str(" result=");
    push_optional_portal_debug_rect(&mut line, clip.result_bounds);
    line.emit();
}

fn debug_log_portal_visible_rooms(result: &RuntimePortalVisibility) {
    let mut line = DebugLogLine::new("portal vis rooms=");
    let limit = result.room_count.min(MAX_ACTIVE_ROOMS);
    let mut i = 0usize;
    while i < limit {
        if i > 0 {
            line.push_byte(b',');
        }
        let room = result.rooms[i];
        line.push_room(room.room);
        line.push_byte(b':');
        line.push_u32(room.depth as u32);
        line.push_byte(b'/');
        line.push_u32(room.frustum_count as u32);
        i += 1;
    }
    line.emit();
}

fn debug_log_portal_visibility_source_portal_summaries(
    camera: PortalVisibilityCamera,
    result: &RuntimePortalVisibility,
) {
    let mut logged = 0usize;
    let frustum_limit = result
        .frustum_count
        .min(PORTAL_VIS_DEBUG_LOG_MAX_FRUSTUMS)
        .min(MAX_PORTAL_FRUSTUMS);
    let mut frustum_slot = 0usize;
    while frustum_slot < frustum_limit && logged < PORTAL_VIS_DEBUG_LOG_MAX_PORTALS {
        let frustum = result.frustums[frustum_slot];
        let Some(record) = ROOMS.get(frustum.room.to_usize()) else {
            frustum_slot += 1;
            continue;
        };
        let portal_first = record.portal_first as usize;
        let portal_end = portal_first.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_first;
        while portal_index < portal_end.min(ROOM_PORTALS.len())
            && logged < PORTAL_VIS_DEBUG_LOG_MAX_PORTALS
        {
            let portal = ROOM_PORTALS[portal_index];
            if portal.source_room == frustum.room {
                let clip = debug_portal_clip(portal, camera, frustum);
                debug_log_portal_clip_summary_line(
                    portal_index,
                    portal,
                    frustum,
                    clip,
                    result.stats,
                );
                logged += 1;
            }
            portal_index += 1;
        }
        frustum_slot += 1;
    }
}

fn debug_log_portal_clip_line(
    root_room: RoomIndex,
    portal_index: usize,
    parent: PortalFrustum,
    portal: psx_level::LevelRoomPortalRecord,
    clip: PortalClipDebug,
    stats: psx_level::portal_visibility::PortalVisibilityStats,
) {
    let portal_bit = portal_debug_mask_bit(portal_index);
    let tested = !portal_bit.is_empty() && stats.tested_portal_mask.contains_index(portal_index);
    let accepted =
        !portal_bit.is_empty() && stats.accepted_portal_mask.contains_index(portal_index);
    let rejected = !portal_bit.is_empty()
        && stats
            .reject_frustum_portal_mask
            .contains_index(portal_index);
    let skip_backlink =
        portal.destination_room == root_room || portal.destination_room == parent.source_room;

    let mut line = DebugLogLine::new("portal p idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" src=");
    line.push_room(portal.source_room);
    line.push_str(" dst=");
    line.push_room(portal.destination_room);
    line.push_str(" depth=");
    line.push_u32(parent.depth as u32);
    line.push_str(" decision=");
    line.push_str(portal_debug_decision_name(clip.decision));
    line.push_str(" flags t/a/r/skip=");
    line.push_bool(tested);
    line.push_byte(b'/');
    line.push_bool(accepted);
    line.push_byte(b'/');
    line.push_bool(rejected);
    line.push_byte(b'/');
    line.push_bool(skip_backlink);
    line.push_str(" front=");
    line.push_bool(clip.front_faces_camera);
    line.emit();

    let mut line = DebugLogLine::new("portal p counts idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" n/l/r/b/t=");
    line.push_u32(clip.near_count as u32);
    line.push_byte(b'/');
    line.push_u32(clip.left_count as u32);
    line.push_byte(b'/');
    line.push_u32(clip.right_count as u32);
    line.push_byte(b'/');
    line.push_u32(clip.bottom_count as u32);
    line.push_byte(b'/');
    line.push_u32(clip.top_count as u32);
    line.push_str(" empty=");
    line.push_str(portal_debug_plane_name(clip.first_empty_plane));
    line.push_str(" tiny=");
    line.push_bool(clip.tiny);
    line.push_str(" normal=(");
    line.push_i32(portal.normal_x as i32);
    line.push_byte(b',');
    line.push_i32(portal.normal_y as i32);
    line.push_byte(b',');
    line.push_i32(portal.normal_z as i32);
    line.push_byte(b')');
    line.emit();

    let mut line = DebugLogLine::new("portal p geom idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    let mut i = 0usize;
    while i < 4 {
        line.push_str(" v");
        line.push_u32(i as u32);
        line.push_byte(b'=');
        line.push_point(RoomPoint::new(
            portal.vertex_x[i],
            portal.vertex_y[i],
            portal.vertex_z[i],
        ));
        i += 1;
    }
    line.emit();

    let mut line = DebugLogLine::new("portal p view idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    let mut i = 0usize;
    while i < 4 {
        line.push_str(" v");
        line.push_u32(i as u32);
        line.push_byte(b'=');
        let vertex = clip.view_vertices[i];
        line.push_point(RoomPoint::new(vertex.x, vertex.y, vertex.z));
        i += 1;
    }
    line.emit();

    let mut line = DebugLogLine::new("portal p clip idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" parent=");
    push_portal_debug_rect(&mut line, clip.parent);
    line.push_str(" proj=");
    push_optional_portal_debug_rect(&mut line, clip.projected_bounds);
    line.push_str(" clipped=");
    push_optional_portal_debug_rect(&mut line, clip.clipped_bounds);
    line.push_str(" result=");
    push_optional_portal_debug_rect(&mut line, clip.result_bounds);
    line.emit();
}

fn debug_log_portal_visibility_source_portals(
    root_room: RoomIndex,
    camera: PortalVisibilityCamera,
    result: &RuntimePortalVisibility,
) {
    let mut logged = 0usize;
    let frustum_limit = result
        .frustum_count
        .min(PORTAL_VIS_DEBUG_LOG_MAX_FRUSTUMS)
        .min(MAX_PORTAL_FRUSTUMS);
    let mut frustum_slot = 0usize;
    while frustum_slot < frustum_limit && logged < PORTAL_VIS_DEBUG_LOG_MAX_PORTALS {
        let frustum = result.frustums[frustum_slot];
        let Some(record) = ROOMS.get(frustum.room.to_usize()) else {
            frustum_slot += 1;
            continue;
        };
        let portal_first = record.portal_first as usize;
        let portal_end = portal_first.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_first;
        while portal_index < portal_end.min(ROOM_PORTALS.len())
            && logged < PORTAL_VIS_DEBUG_LOG_MAX_PORTALS
        {
            let portal = ROOM_PORTALS[portal_index];
            if portal.source_room == frustum.room {
                let clip = debug_portal_clip(portal, camera, frustum);
                debug_log_portal_clip_line(
                    root_room,
                    portal_index,
                    frustum,
                    portal,
                    clip,
                    result.stats,
                );
                logged += 1;
            }
            portal_index += 1;
        }
        frustum_slot += 1;
    }
}

fn should_debug_log_portal_visibility(
    current_record: &LevelRoomRecord,
    result: &RuntimePortalVisibility,
) -> bool {
    let stats = result.stats;
    stats.reject_backface != 0
        || stats.reject_frustum != 0
        || stats.reject_tiny != 0
        || stats.cap_room != 0
        || stats.cap_frustum != 0
        || stats.cap_depth != 0
        || (current_record.portal_count != 0 && current_record.portal_count <= 4)
}

fn debug_log_portal_visibility_snapshot(
    current_room: RoomIndex,
    current_record: &LevelRoomRecord,
    player_room: RoomIndex,
    player_local: RoomPoint,
    player_global: RoomPoint,
    view: ActiveRoomView,
    camera: PortalVisibilityCamera,
    result: &RuntimePortalVisibility,
) {
    if !should_debug_log_portal_visibility(current_record, result) {
        return;
    }
    debug_log_portal_visibility_summary(
        current_room,
        player_room,
        player_local,
        player_global,
        view,
        camera,
        result,
    );
    debug_log_portal_visible_rooms(result);
    debug_log_portal_visibility_source_portal_summaries(camera, result);
    if PORTAL_VIS_DEBUG_VERBOSE_CLIPS {
        debug_log_portal_visibility_source_portals(current_room, camera, result);
    }
}

fn active_room_cache_status_debug_code(status: ActiveRoomCacheStatus) -> u32 {
    match status {
        ActiveRoomCacheStatus::Ready => 0,
        ActiveRoomCacheStatus::NotBuilt => 1,
        ActiveRoomCacheStatus::Overflow => 2,
        ActiveRoomCacheStatus::Empty => 3,
    }
}

fn debug_log_post_cross_render_start(
    room: RoomIndex,
    camera: WorldCamera,
    visible_mask: RuntimeDebugMask,
    active_mask: RuntimeDebugMask,
    current_collision_ready: bool,
) {
    let mut line = DebugLogLine::new("render start room=");
    line.push_room(room);
    line.push_str(" cam=");
    line.push_point(RoomPoint::new(
        camera.position.x,
        camera.position.y,
        camera.position.z,
    ));
    line.push_str(" vis=");
    line.push_hex_mask(visible_mask);
    line.push_str(" active=");
    line.push_hex_mask(active_mask);
    line.push_str(" coll=");
    line.push_bool(current_collision_ready);
    line.emit();
}

fn debug_log_post_cross_render_room(slot: usize, active: ActiveRuntimeRoom, draws: bool) {
    let cache = active.surface_cache;
    let mut line = DebugLogLine::new("render room slot=");
    line.push_u32(slot.min(u32::MAX as usize) as u32);
    line.push_str(" room=");
    line.push_room(active.index);
    line.push_str(" stream=");
    line.push_u32(active.stream_slot as u32);
    line.push_str(" off=(");
    line.push_i32(active.offset_x);
    line.push_byte(b',');
    line.push_i32(active.offset_z);
    line.push_byte(b')');
    line.push_str(" draw=");
    line.push_bool(draws);
    line.push_str(" cache=");
    line.push_bool(cache.ready);
    line.push_str(" st=");
    line.push_u32(active_room_cache_status_debug_code(cache.status));
    line.push_str(" cells=");
    line.push_u32(cache.cell_count.min(u32::MAX as usize) as u32);
    line.push_str(" verts=");
    line.push_u32(cache.vertex_count.min(u32::MAX as usize) as u32);
    line.push_str(" surf=");
    line.push_u32(cache.surface_count.min(u32::MAX as usize) as u32);
    line.push_str(" rr=");
    line.push_bool(active.render_room.is_some());
    line.push_str(" slices=");
    line.push_bool(room_surface_cache_slices(active.index, cache).is_some());
    line.emit();
}

fn debug_log_post_cross_render_end(
    room: RoomIndex,
    active_mask: RuntimeDebugMask,
    drawn_mask: RuntimeDebugMask,
    primitive_count: usize,
    primitive_remaining: usize,
    world_commands: usize,
) {
    let mut line = DebugLogLine::new("render end room=");
    line.push_room(room);
    line.push_str(" active=");
    line.push_hex_mask(active_mask);
    line.push_str(" drawn=");
    line.push_hex_mask(drawn_mask);
    line.push_str(" prim=");
    line.push_u32(primitive_count.min(u32::MAX as usize) as u32);
    line.push_str(" rem=");
    line.push_u32(primitive_remaining.min(u32::MAX as usize) as u32);
    line.push_str(" cmd=");
    line.push_u32(world_commands.min(u32::MAX as usize) as u32);
    line.emit();
}

#[cfg(feature = "cd-stream-bench")]
fn debug_log_stream_plan<const N: usize>(label: &str, plan: &RoomStreamLoadPlan<N>) {
    let mut line = DebugLogLine::new(label);
    line.push_str(" count=");
    line.push_u32(plan.count.min(u32::MAX as usize) as u32);
    line.push_str(" rooms=");
    let limit = plan.count.min(N).min(STREAMED_ROOM_SLOT_COUNT);
    let mut i = 0usize;
    while i < limit {
        if i > 0 {
            line.push_byte(b',');
        }
        line.push_room(plan.rooms[i]);
        line.push_byte(b'@');
        line.push_u32(plan.slots[i].min(u32::MAX as usize) as u32);
        i += 1;
    }
    line.emit();
}

#[cfg(feature = "cd-stream-bench")]
fn debug_log_stream_entry(
    label: &str,
    room: RoomIndex,
    slot: usize,
    byte_count: usize,
    status: u32,
) {
    let mut line = DebugLogLine::new(label);
    line.push_str(" room=");
    line.push_room(room);
    line.push_str(" slot=");
    line.push_u32(slot.min(u32::MAX as usize) as u32);
    line.push_str(" bytes=");
    line.push_u32(byte_count.min(u32::MAX as usize) as u32);
    line.push_str(" status=");
    line.push_u32(status);
    line.emit();
}

fn encode_debug_map_position(value: i32) -> u32 {
    let encoded = value.saturating_add(DEBUG_MAP_POSITION_BIAS);
    if encoded < 0 {
        0
    } else {
        encoded as u32
    }
}

fn encode_debug_q12_basis(value: i32) -> u32 {
    value.saturating_add(4096).clamp(0, 8192) as u32
}

fn emit_player_map_debug(
    room: RoomIndex,
    position: RoomPoint,
    camera_position: RoomPoint,
    camera_global: RoomPoint,
    view_yaw_q12: u16,
    view_sin_yaw_q12: i32,
    view_cos_yaw_q12: i32,
    view_sin_pitch_q12: i32,
    view_cos_pitch_q12: i32,
) {
    telemetry::counter(
        telemetry::counter::ROOM_PLAYER_ROOM_INDEX,
        room.raw() as u32,
    );
    telemetry::counter(
        telemetry::counter::ROOM_PLAYER_LOCAL_X_BIASED,
        encode_debug_map_position(position.x),
    );
    telemetry::counter(
        telemetry::counter::ROOM_PLAYER_LOCAL_Z_BIASED,
        encode_debug_map_position(position.z),
    );
    telemetry::counter(
        telemetry::counter::ROOM_PLAYER_VIEW_YAW_Q12,
        view_yaw_q12 as u32,
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_LOCAL_X_BIASED,
        encode_debug_map_position(camera_position.x),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_LOCAL_Y_BIASED,
        encode_debug_map_position(camera_position.y),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_LOCAL_Z_BIASED,
        encode_debug_map_position(camera_position.z),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_GLOBAL_X_BIASED,
        encode_debug_map_position(camera_global.x),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_GLOBAL_Y_BIASED,
        encode_debug_map_position(camera_global.y),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_GLOBAL_Z_BIASED,
        encode_debug_map_position(camera_global.z),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED,
        encode_debug_q12_basis(view_sin_yaw_q12),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED,
        encode_debug_q12_basis(view_cos_yaw_q12),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_VIEW_SIN_PITCH_Q12_BIASED,
        encode_debug_q12_basis(view_sin_pitch_q12),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_VIEW_COS_PITCH_Q12_BIASED,
        encode_debug_q12_basis(view_cos_pitch_q12),
    );
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
/// Graph-distance (BFS) radius for the streaming residency ring. Rooms within
/// this many portal hops of the current room are kept resident in memory
/// (the load-ahead prefetch buffer). `WORLD_STREAM_RADIUS >= WORLD_VISIBILITY_RADIUS`.
/// 16 covers demo10's 8-room graph, so the streaming ring is every room and the
/// runtime stays equivalent to today's all-resident baseline.
const WORLD_STREAM_RADIUS: u16 = 16;
/// Graph-distance (BFS) radius for the surface-cache build ring. Rooms within
/// this many portal hops of the current room get their surface caches built;
/// the portal-visibility/frustum pass still decides which built rooms draw.
const WORLD_VISIBILITY_RADIUS: u16 = 16;
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

/// Capacity of the residency manager's RAM table. Holds room
/// world + model meshes + animation clips.
const MAX_RESIDENT_RAM_ASSETS: usize = 128;
/// Capacity of the residency manager's VRAM table. Holds room
/// material atlases + model atlases.
const MAX_RESIDENT_VRAM_ASSETS: usize = 64;

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

/// Marker visualization tuning. Markers are debug stubs -- keep
/// them visible at orbit-camera scales without dominating the
/// scene.
const MARKER_HALF: i32 = 96;
const MARKER_LIFT: i32 = MARKER_HALF;
const MARKER_TINT: (u8, u8, u8) = (0xff, 0xa8, 0x40);
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

/// Residency manager -- tracks which AssetIds are RAM/VRAM
/// resident across frames. Static so it survives across the
/// `Scene::init` → `Scene::render` boundary.
static mut RESIDENCY: ResidencyManager<MAX_RESIDENT_RAM_ASSETS, MAX_RESIDENT_VRAM_ASSETS> =
    ResidencyManager::new();

/// Per-asset upload bookkeeping. When a texture asset becomes
/// VRAM-resident we record its CLUT word, tpage word, and texture
/// window so the per-frame material build can reconstruct its
/// `TextureMaterial` without re-walking the upload code.
#[derive(Copy, Clone)]
struct VramSlot {
    asset: AssetId,
    clut_mode: VramSlotClutMode,
    ready: bool,
    clut_word: u16,
    tpage_word: u16,
    texture_window: TextureWindow,
    texture_width: u16,
    texture_height: u16,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum VramSlotClutMode {
    OpaqueZero,
    TransparentZero,
    ModelAtlas,
    SkyPanorama,
}

const VRAM_SLOT_EMPTY: Option<VramSlot> = None;
static mut VRAM_SLOTS: [Option<VramSlot>; MAX_RESIDENT_VRAM_ASSETS] =
    [VRAM_SLOT_EMPTY; MAX_RESIDENT_VRAM_ASSETS];
/// Number of VRAM slots used so far across room textures and model atlases.
static mut VRAM_SLOT_COUNT: usize = 0;
/// Number of room texture CLUT slots uploaded. A texture may consume
/// two CLUT slots when used both as opaque room geometry and as a
/// zero-transparent image prop, while sharing one pixel upload.
/// Pixel placement is tracked by `ROOM_TEXTURE_ALLOCATOR`.
/// Kept separate from `VRAM_SLOT_COUNT` so model atlas uploads cannot
/// shift room texture addressing.
static mut ROOM_TEXTURE_COUNT: usize = 0;
static mut ROOM_TEXTURE_ALLOCATOR: TextureWindowAtlas<ROOM_TPAGE_COUNT> = TextureWindowAtlas::new();

/// Tpage X cursor (in halfwords) for the model-atlas 8bpp
/// region. Distinct cursor so room-material uploads don't shift
/// model atlas positions and vice versa.
static mut MODEL_TPAGE_X_CURSOR: u16 = 0;
/// Number of model atlases uploaded so far. Doubles as the
/// CLUT row offset: each 8bpp atlas needs a fresh 256-entry
/// CLUT row.
static mut MODEL_ATLAS_COUNT: usize = 0;

const VRAM_UPLOAD_QUEUE_CAP: usize = 8;
const VRAM_UPLOAD_ROWS_PER_BACKGROUND_TICK: u16 = 8;
const ROOM_WINDOW_BACKGROUND_TICK_MASK: u32 = 1;

#[derive(Copy, Clone, PartialEq, Eq)]
enum VramUploadKind {
    TextureAndClut,
    ClutOnly,
}

#[derive(Copy, Clone)]
struct VramUploadJob {
    active: bool,
    slot_index: u16,
    asset: AssetId,
    clut_mode: VramSlotClutMode,
    kind: VramUploadKind,
    bytes: Option<&'static [u8]>,
    texture_x: u16,
    texture_y: u16,
    texture_width_halfwords: u16,
    texture_height_rows: u16,
    next_texture_row: u16,
    clut_x: u16,
    clut_y: u16,
    clut_entries: u16,
    clut_uploaded: bool,
}

impl VramUploadJob {
    const EMPTY: Self = Self {
        active: false,
        slot_index: 0,
        asset: AssetId(0),
        clut_mode: VramSlotClutMode::OpaqueZero,
        kind: VramUploadKind::TextureAndClut,
        bytes: None,
        texture_x: 0,
        texture_y: 0,
        texture_width_halfwords: 0,
        texture_height_rows: 0,
        next_texture_row: 0,
        clut_x: 0,
        clut_y: 0,
        clut_entries: 0,
        clut_uploaded: false,
    };

    fn texture_complete(self) -> bool {
        self.kind == VramUploadKind::ClutOnly || self.next_texture_row >= self.texture_height_rows
    }

    fn complete(self) -> bool {
        self.texture_complete() && self.clut_uploaded
    }
}

struct VramUploadQueue {
    jobs: [VramUploadJob; VRAM_UPLOAD_QUEUE_CAP],
}

impl VramUploadQueue {
    const fn new() -> Self {
        Self {
            jobs: [VramUploadJob::EMPTY; VRAM_UPLOAD_QUEUE_CAP],
        }
    }

    fn contains(&self, asset: AssetId, clut_mode: VramSlotClutMode) -> bool {
        let mut i = 0usize;
        while i < self.jobs.len() {
            let job = self.jobs[i];
            if job.active && job.asset == asset && job.clut_mode == clut_mode {
                return true;
            }
            i += 1;
        }
        false
    }

    fn has_free_slot(&self) -> bool {
        let mut i = 0usize;
        while i < self.jobs.len() {
            if !self.jobs[i].active {
                return true;
            }
            i += 1;
        }
        false
    }

    fn push(&mut self, job: VramUploadJob) -> bool {
        let mut i = 0usize;
        while i < self.jobs.len() {
            if !self.jobs[i].active {
                self.jobs[i] = job;
                return true;
            }
            i += 1;
        }
        false
    }

    fn step(&mut self, row_budget: u16) -> bool {
        let mut remaining_rows = row_budget;
        let mut completed_any = false;
        let mut i = 0usize;
        while i < self.jobs.len() && remaining_rows > 0 {
            if !self.jobs[i].active {
                i += 1;
                continue;
            }

            telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
            if !self.jobs[i].texture_complete() {
                let rows = self.upload_texture_rows(i, remaining_rows);
                remaining_rows = remaining_rows.saturating_sub(rows.max(1));
            } else if !self.jobs[i].clut_uploaded {
                self.upload_clut(i);
                remaining_rows = remaining_rows.saturating_sub(1);
            }
            telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);

            if self.jobs[i].complete() {
                unsafe {
                    mark_vram_slot_ready(self.jobs[i].slot_index as usize);
                }
                telemetry::counter(telemetry::counter::ROOM_TEXTURE_UPLOADS, 1);
                self.jobs[i] = VramUploadJob::EMPTY;
                completed_any = true;
            }
            i += 1;
        }
        completed_any
    }

    fn upload_texture_rows(&mut self, index: usize, row_budget: u16) -> u16 {
        let Some(bytes) = self.jobs[index].bytes else {
            self.jobs[index] = VramUploadJob::EMPTY;
            return 0;
        };
        let Some(texture) = Texture::from_bytes(bytes).ok() else {
            self.jobs[index] = VramUploadJob::EMPTY;
            return 0;
        };
        let row_bytes = usize::from(self.jobs[index].texture_width_halfwords).saturating_mul(2);
        if row_bytes == 0
            || texture.pixel_bytes().len()
                < row_bytes.saturating_mul(usize::from(self.jobs[index].texture_height_rows))
        {
            self.jobs[index] = VramUploadJob::EMPTY;
            return 0;
        }

        let mut uploaded = 0u16;
        while uploaded < row_budget
            && self.jobs[index].next_texture_row < self.jobs[index].texture_height_rows
        {
            let row = self.jobs[index].next_texture_row;
            let offset = usize::from(row).saturating_mul(row_bytes);
            upload_bytes(
                VramRect::new(
                    self.jobs[index].texture_x,
                    self.jobs[index].texture_y.saturating_add(row),
                    self.jobs[index].texture_width_halfwords,
                    1,
                ),
                &texture.pixel_bytes()[offset..offset + row_bytes],
            );
            self.jobs[index].next_texture_row = self.jobs[index].next_texture_row.saturating_add(1);
            uploaded = uploaded.saturating_add(1);
        }
        uploaded
    }

    fn upload_clut(&mut self, index: usize) {
        let Some(bytes) = self.jobs[index].bytes else {
            self.jobs[index] = VramUploadJob::EMPTY;
            return;
        };
        let Some(texture) = Texture::from_bytes(bytes).ok() else {
            self.jobs[index] = VramUploadJob::EMPTY;
            return;
        };
        let clut_bytes = texture.clut_bytes();
        let expected_len = usize::from(self.jobs[index].clut_entries).saturating_mul(2);
        if clut_bytes.len() < expected_len {
            self.jobs[index] = VramUploadJob::EMPTY;
            return;
        }
        let rect = VramRect::new(
            self.jobs[index].clut_x,
            self.jobs[index].clut_y,
            self.jobs[index].clut_entries,
            1,
        );
        if self.jobs[index].clut_mode == VramSlotClutMode::OpaqueZero {
            upload_opaque_clut(rect, &clut_bytes[..expected_len]);
        } else {
            upload_clut(rect, &clut_bytes[..expected_len]);
        }
        self.jobs[index].clut_uploaded = true;
    }
}

static mut VRAM_UPLOAD_QUEUE: VramUploadQueue = VramUploadQueue::new();

#[derive(Copy, Clone)]
struct RuntimeStreamingJobs {
    vram_rows_per_tick: u16,
}

impl RuntimeStreamingJobs {
    const fn new() -> Self {
        Self {
            vram_rows_per_tick: VRAM_UPLOAD_ROWS_PER_BACKGROUND_TICK,
        }
    }

    fn background_tick(self, ctx: &Ctx) -> bool {
        (ctx.sim_tick.as_u32() & ROOM_WINDOW_BACKGROUND_TICK_MASK) != 0
    }

    fn step_vram_uploads(self) -> bool {
        unsafe { VRAM_UPLOAD_QUEUE.step(self.vram_rows_per_tick) }
    }
}

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
struct BoxPropBreakEvent {
    prop_index: u16,
    age: u8,
    impulse_x_q8: i16,
    impulse_z_q8: i16,
}

impl BoxPropBreakEvent {
    const EMPTY: Self = Self {
        prop_index: u16::MAX,
        age: 0,
        impulse_x_q8: 0,
        impulse_z_q8: 0,
    };

    const fn is_active(self) -> bool {
        self.prop_index != u16::MAX
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct BoxPropBreakShard {
    face: u8,
    u0_q8: u16,
    v0_q8: u16,
    u1_q8: u16,
    v1_q8: u16,
    drift_q8_per_frame: i8,
    lift_per_frame: i8,
    impulse_per_frame: u8,
    twist_q8_per_frame: i8,
    delay: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct BoxPropFloorDebrisChip {
    face: u8,
    offset_x_q8: i16,
    offset_z_q8: i16,
    half_length_q8: u16,
    half_width_q8: u16,
    yaw_q12: u16,
    u0_q8: u16,
    v0_q8: u16,
    u1_q8: u16,
    v1_q8: u16,
    lift: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct BoxPropDebrisBounds {
    center_x: i32,
    center_z: i32,
    span_x: i32,
    span_z: i32,
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

/// Parsed, VRAM-bound model payload ready for the hot render path.
#[derive(Copy, Clone)]
struct RuntimeModelAsset {
    index: ModelIndex,
    model: Model<'static>,
    material: TextureMaterial,
    clip_first: ModelClipTableIndex,
    clip_count: u16,
    default_clip: ModelClipIndex,
    socket_first: ModelSocketIndex,
    socket_count: u16,
    face_first: u16,
    face_count: u16,
    part_first: u16,
    part_count: u16,
    vertex_first: u16,
    vertex_count: u16,
    requires_cpu_blend: bool,
    world_height: u16,
    collision_radius: u16,
    local_to_world: LocalToWorldScale,
}

impl RuntimeModelAsset {
    fn from_record(
        index: ModelIndex,
        record: &LevelModelRecord,
        face_pool: &mut [TexturedModelRenderFace],
        face_cursor: &mut usize,
        part_pool: &mut [ModelPart],
        part_cursor: &mut usize,
        vertex_pool: &mut [ModelVertex],
        vertex_cursor: &mut usize,
    ) -> Option<Self> {
        let mesh_asset = find_asset_of_kind(ASSETS, record.mesh_asset, AssetKind::ModelMesh)?;
        let model = Model::from_bytes(mesh_asset.bytes).ok()?;
        let texture_asset = record.texture_asset?;
        let atlas_asset = find_asset_of_kind(ASSETS, texture_asset, AssetKind::Texture)?;
        let atlas_slot = ensure_model_atlas_uploaded(atlas_asset.id, atlas_asset.bytes)?;
        let mut next_face_cursor = *face_cursor;
        let face_first = next_face_cursor;
        let face_count = decode_model_render_faces(
            model,
            atlas_slot.texture_width,
            atlas_slot.texture_height,
            face_pool,
            &mut next_face_cursor,
        )?;
        let mut next_part_cursor = *part_cursor;
        let mut next_vertex_cursor = *vertex_cursor;
        let (part_first, part_count, vertex_first, vertex_count) = decode_model_render_geometry(
            model,
            part_pool,
            &mut next_part_cursor,
            vertex_pool,
            &mut next_vertex_cursor,
        )?;
        *face_cursor = next_face_cursor;
        *part_cursor = next_part_cursor;
        *vertex_cursor = next_vertex_cursor;
        Some(Self {
            index,
            model,
            material: TextureMaterial::opaque(
                atlas_slot.clut_word,
                atlas_slot.tpage_word,
                (0x80, 0x80, 0x80),
            ),
            clip_first: record.clip_first,
            clip_count: record.clip_count,
            default_clip: record.default_clip,
            socket_first: record.socket_first,
            socket_count: record.socket_count,
            face_first: face_first as u16,
            face_count: face_count as u16,
            part_first: part_first as u16,
            part_count: part_count as u16,
            vertex_first: vertex_first as u16,
            vertex_count: vertex_count as u16,
            requires_cpu_blend: model_requires_cpu_blend(model),
            world_height: record.world_height,
            collision_radius: record.collision_radius,
            local_to_world: LocalToWorldScale::from_q12(model.local_to_world_q12()),
        })
    }

    fn clip(
        self,
        clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
        local_clip: ModelClipIndex,
    ) -> Option<Animation<'static>> {
        let index = self.clip_table_index(local_clip)?.to_usize();
        clips.get(index).copied().flatten()
    }

    fn clip_table_index(self, local_clip: ModelClipIndex) -> Option<ModelClipTableIndex> {
        if local_clip.raw() >= self.clip_count {
            return None;
        }
        Some(ModelClipTableIndex(
            self.clip_first.raw().saturating_add(local_clip.raw()),
        ))
    }
}

fn model_requires_cpu_blend(model: Model<'_>) -> bool {
    let joint_count = model.joint_count() as usize;
    let mut i = 0u16;
    while i < model.vertex_count() {
        if let Some(vertex) = model.vertex(i) {
            if vertex.is_blend() && (vertex.joint1 as usize) < joint_count {
                return true;
            }
        }
        i = i.saturating_add(1);
    }
    false
}

fn decode_model_render_faces(
    model: Model<'_>,
    texture_width: u16,
    texture_height: u16,
    face_pool: &mut [TexturedModelRenderFace],
    face_cursor: &mut usize,
) -> Option<usize> {
    let face_count = model.face_count() as usize;
    if face_count > u16::MAX as usize || face_pool.len().saturating_sub(*face_cursor) < face_count {
        return None;
    }

    let (max_u, max_v) = model_render_uv_limits(texture_width, texture_height);
    let mut i = 0usize;
    while i < face_count {
        let face = model.face(i as u16)?;
        face_pool[*face_cursor + i] = TexturedModelRenderFace::new(
            [
                face.corners[0].vertex_index,
                face.corners[1].vertex_index,
                face.corners[2].vertex_index,
            ],
            [
                clamp_model_render_uv(face.corners[0].uv, max_u, max_v),
                clamp_model_render_uv(face.corners[1].uv, max_u, max_v),
                clamp_model_render_uv(face.corners[2].uv, max_u, max_v),
            ],
        );
        i += 1;
    }
    *face_cursor += face_count;
    Some(face_count)
}

fn decode_model_render_geometry(
    model: Model<'_>,
    part_pool: &mut [ModelPart],
    part_cursor: &mut usize,
    vertex_pool: &mut [ModelVertex],
    vertex_cursor: &mut usize,
) -> Option<(usize, usize, usize, usize)> {
    let part_count = model.part_count() as usize;
    let vertex_count = model.vertex_count() as usize;
    if part_count > u16::MAX as usize
        || vertex_count > u16::MAX as usize
        || part_pool.len().saturating_sub(*part_cursor) < part_count
        || vertex_pool.len().saturating_sub(*vertex_cursor) < vertex_count
    {
        return None;
    }

    let part_first = *part_cursor;
    let vertex_first = *vertex_cursor;
    let mut i = 0usize;
    while i < part_count {
        part_pool[part_first + i] = model.part(i as u16)?;
        i += 1;
    }
    i = 0;
    while i < vertex_count {
        vertex_pool[vertex_first + i] = model.vertex(i as u16)?;
        i += 1;
    }
    *part_cursor += part_count;
    *vertex_cursor += vertex_count;
    Some((part_first, part_count, vertex_first, vertex_count))
}

fn square_i32_saturating(value: i32) -> i32 {
    let abs = value.saturating_abs();
    if abs > 46_340 {
        i32::MAX
    } else {
        abs * abs
    }
}

fn min_i32x4(values: [i32; 4]) -> i32 {
    values[0].min(values[1]).min(values[2]).min(values[3])
}

fn max_i32x4(values: [i32; 4]) -> i32 {
    values[0].max(values[1]).max(values[2]).max(values[3])
}

fn model_render_uv_limits(texture_width: u16, texture_height: u16) -> (u8, u8) {
    (
        model_render_uv_max(texture_width),
        model_render_uv_max(texture_height),
    )
}

fn model_render_uv_max(size: u16) -> u8 {
    size.saturating_sub(1).min(u16::from(u8::MAX)) as u8
}

fn clamp_model_render_uv(uv: (u8, u8), max_u: u8, max_v: u8) -> (u8, u8) {
    (uv.0.min(max_u), uv.1.min(max_v))
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

fn runtime_model_faces<'a>(
    model: RuntimeModelAsset,
    face_pool: &'a [TexturedModelRenderFace],
) -> &'a [TexturedModelRenderFace] {
    let first = model.face_first as usize;
    let count = model.face_count as usize;
    let end = first.saturating_add(count).min(face_pool.len());
    if first >= end || first >= face_pool.len() {
        &[]
    } else {
        &face_pool[first..end]
    }
}

fn runtime_model_geometry<'a>(
    model: RuntimeModelAsset,
    part_pool: &'a [ModelPart],
    vertex_pool: &'a [ModelVertex],
) -> Option<TexturedModelGeometry<'a>> {
    let part_first = model.part_first as usize;
    let part_count = model.part_count as usize;
    let vertex_first = model.vertex_first as usize;
    let vertex_count = model.vertex_count as usize;
    if part_count == 0 || vertex_count == 0 {
        return None;
    }
    let part_end = part_first.checked_add(part_count)?;
    let vertex_end = vertex_first.checked_add(vertex_count)?;
    let parts = part_pool.get(part_first..part_end)?;
    let vertices = vertex_pool.get(vertex_first..vertex_end)?;
    Some(TexturedModelGeometry::new(parts, vertices))
}

#[derive(Copy, Clone)]
struct ActiveRoomSurfaceCache {
    cell_first: usize,
    cell_count: usize,
    cell_vertex_first: usize,
    cell_vertex_count: usize,
    vertex_first: usize,
    vertex_count: usize,
    surface_first: usize,
    surface_count: usize,
    status: ActiveRoomCacheStatus,
    ready: bool,
}

impl ActiveRoomSurfaceCache {
    const EMPTY: Self = Self {
        cell_first: 0,
        cell_count: 0,
        cell_vertex_first: 0,
        cell_vertex_count: 0,
        vertex_first: 0,
        vertex_count: 0,
        surface_first: 0,
        surface_count: 0,
        status: ActiveRoomCacheStatus::NotBuilt,
        ready: false,
    };
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum ActiveRoomCacheStatus {
    Ready,
    NotBuilt,
    Overflow,
    Empty,
}

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
struct StreamedRoomSlot {
    room: RoomIndex,
    byte_count: usize,
    last_used: u32,
    state: RoomStreamSlotState,
}

#[cfg(feature = "cd-stream-bench")]
impl StreamedRoomSlot {
    const EMPTY: Self = Self {
        room: INVALID_ROOM_INDEX,
        byte_count: 0,
        last_used: 0,
        state: RoomStreamSlotState::Empty,
    };
}

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone, PartialEq, Eq)]
enum RoomStreamSlotState {
    Empty,
    Resident,
    Loading,
    Failed,
}

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
struct RoomStreamLoadPlan<const N: usize> {
    rooms: [RoomIndex; N],
    slots: [usize; N],
    count: usize,
}

#[cfg(feature = "cd-stream-bench")]
impl<const N: usize> RoomStreamLoadPlan<N> {
    const EMPTY: Self = Self {
        rooms: [INVALID_ROOM_INDEX; N],
        slots: [usize::MAX; N],
        count: 0,
    };
}

#[cfg(feature = "cd-stream-bench")]
struct RoomStreamScheduler<const N: usize> {
    slots: [StreamedRoomSlot; N],
    room_slots: [u16; MAX_STREAMED_ROOM_INDEX_COUNT],
    /// Rooms declared part of the resident window via `set_resident_window`.
    /// Pinned rooms are never chosen for eviction regardless of LRU age, so the
    /// residency owner can keep them resident without re-requesting them. This
    /// is the primitive both policies build on: full-residency pins every room,
    /// a sliding window pins the current room plus its near neighbours.
    pinned_rooms: [bool; MAX_STREAMED_ROOM_INDEX_COUNT],
    job: cd_stream::WorldRoomSlotsReadJob<N>,
    job_plan: RoomStreamLoadPlan<N>,
    slot_limit: usize,
    epoch: u32,
    window_requests: u16,
    window_misses: u16,
    window_prefetch_requests: u16,
    window_evictions: u16,
    window_failed_loads: u16,
    window_pending_loads: u16,
    window_protected_full: u16,
}

#[cfg(feature = "cd-stream-bench")]
impl<const N: usize> RoomStreamScheduler<N> {
    const fn new() -> Self {
        Self {
            slots: [StreamedRoomSlot::EMPTY; N],
            room_slots: [STREAMED_ROOM_SLOT_NONE; MAX_STREAMED_ROOM_INDEX_COUNT],
            pinned_rooms: [false; MAX_STREAMED_ROOM_INDEX_COUNT],
            job: cd_stream::WorldRoomSlotsReadJob::new(),
            job_plan: RoomStreamLoadPlan::EMPTY,
            slot_limit: N,
            epoch: 0,
            window_requests: 0,
            window_misses: 0,
            window_prefetch_requests: 0,
            window_evictions: 0,
            window_failed_loads: 0,
            window_pending_loads: 0,
            window_protected_full: 0,
        }
    }

    fn effective_slot_limit(&self) -> usize {
        self.slot_limit.clamp(1, N)
    }

    fn is_room_pinned(&self, room: RoomIndex) -> bool {
        let index = room.to_usize();
        index < MAX_STREAMED_ROOM_INDEX_COUNT && self.pinned_rooms[index]
    }

    /// Declare the rooms that must stay resident. They are pinned (never
    /// evicted) so they survive without being re-requested; rooms no longer in
    /// the set are unpinned and become evictable again.
    fn set_resident_window(&mut self, rooms: &[RoomIndex], count: usize) {
        self.pinned_rooms = [false; MAX_STREAMED_ROOM_INDEX_COUNT];
        let mut i = 0usize;
        while i < count {
            let index = rooms[i].to_usize();
            if index < MAX_STREAMED_ROOM_INDEX_COUNT {
                self.pinned_rooms[index] = true;
            }
            i += 1;
        }
    }

    /// Single residency entry point: pin the desired set and load whatever is
    /// missing. Called once per frame by the residency owner so residency is
    /// no longer requested ad-hoc from the build paths.
    fn reconcile_residency(
        &mut self,
        desired: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        count: usize,
    ) {
        self.begin_window();
        self.set_resident_window(desired, count);
        let plan = self.plan_window_loads(desired, count, count);
        self.start_load_plan(plan);
        self.emit_counters();
    }

    fn begin_window(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.window_requests = 0;
        self.window_misses = 0;
        self.window_prefetch_requests = 0;
        self.window_evictions = 0;
        self.window_failed_loads = 0;
        self.window_pending_loads = 0;
        self.window_protected_full = 0;
    }

    fn resident_slot_for(&mut self, room: RoomIndex) -> Option<usize> {
        if let Some(slot) = self.mapped_slot_for(room, RoomStreamSlotState::Resident) {
            self.slots[slot].last_used = self.epoch;
            return Some(slot);
        }
        None
    }

    fn is_resident(&self, room: RoomIndex) -> bool {
        self.mapped_slot_for(room, RoomStreamSlotState::Resident)
            .is_some()
    }

    fn resident_byte_count(&self, slot: usize) -> Option<usize> {
        if slot >= self.effective_slot_limit() {
            return None;
        }
        let meta = *self.slots.get(slot)?;
        if meta.state == RoomStreamSlotState::Resident && meta.byte_count > 0 {
            Some(meta.byte_count)
        } else {
            None
        }
    }

    fn loading_slot_for(&self, room: RoomIndex) -> Option<usize> {
        self.mapped_slot_for(room, RoomStreamSlotState::Loading)
    }

    fn is_loading(&self, room: RoomIndex) -> bool {
        self.loading_slot_for(room).is_some()
    }

    fn mapped_slot_for(&self, room: RoomIndex, state: RoomStreamSlotState) -> Option<usize> {
        let room_index = room.to_usize();
        if room_index >= MAX_STREAMED_ROOM_INDEX_COUNT {
            return None;
        }
        let slot = self.room_slots[room_index] as usize;
        if slot >= self.effective_slot_limit() {
            return None;
        }
        let meta = self.slots[slot];
        if meta.room == room && meta.state == state {
            Some(slot)
        } else {
            None
        }
    }

    fn set_slot(&mut self, slot: usize, meta: StreamedRoomSlot) {
        if slot >= N {
            return;
        }
        let old_room = self.slots[slot].room.to_usize();
        if old_room < MAX_STREAMED_ROOM_INDEX_COUNT && self.room_slots[old_room] as usize == slot {
            self.room_slots[old_room] = STREAMED_ROOM_SLOT_NONE;
        }
        self.slots[slot] = meta;
        let new_room = meta.room.to_usize();
        if meta.state != RoomStreamSlotState::Empty && new_room < MAX_STREAMED_ROOM_INDEX_COUNT {
            self.room_slots[new_room] = slot as u16;
        }
    }

    fn plan_window_loads(
        &mut self,
        requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        requested_count: usize,
        active_count: usize,
    ) -> RoomStreamLoadPlan<N> {
        let mut plan = RoomStreamLoadPlan::EMPTY;
        if requested_count > 0 && !self.current_room_request_can_wait(requested_rooms[0]) {
            self.abort_active_load();
        }
        let can_schedule_new_loads = !self.job.is_active();
        let protected_count = active_count
            .min(requested_count)
            .min(self.effective_slot_limit())
            .min(N)
            .min(STREAMED_ROOM_SLOT_COUNT);
        let limit = requested_count
            .min(self.effective_slot_limit())
            .min(N)
            .min(STREAMED_ROOM_SLOT_COUNT);
        let mut i = 0usize;
        while i < limit {
            let room = requested_rooms[i];
            if room == INVALID_ROOM_INDEX {
                i += 1;
                continue;
            }
            self.window_requests = self.window_requests.saturating_add(1);
            if i >= active_count {
                self.window_prefetch_requests = self.window_prefetch_requests.saturating_add(1);
            }
            if self.resident_slot_for(room).is_some() {
                i += 1;
                continue;
            }
            if self.loading_slot_for(room).is_some() {
                self.window_misses = self.window_misses.saturating_add(1);
                self.window_pending_loads = self.window_pending_loads.saturating_add(1);
                i += 1;
                continue;
            }

            self.window_misses = self.window_misses.saturating_add(1);
            if !can_schedule_new_loads {
                i += 1;
                continue;
            }
            if plan.count >= RUNTIME_SCHEDULE.stream_load_batch_count {
                i += 1;
                continue;
            }
            let allow_eviction = i < protected_count;
            let Some(target) = self.choose_slot(
                requested_rooms,
                protected_count,
                &plan.slots,
                plan.count,
                allow_eviction,
            ) else {
                self.window_protected_full = self.window_protected_full.saturating_add(1);
                i += 1;
                continue;
            };
            if self.slots[target].state == RoomStreamSlotState::Resident {
                self.window_evictions = self.window_evictions.saturating_add(1);
            }
            self.set_slot(
                target,
                StreamedRoomSlot {
                    room,
                    byte_count: 0,
                    last_used: self.epoch,
                    state: RoomStreamSlotState::Loading,
                },
            );
            plan.rooms[plan.count] = room;
            plan.slots[plan.count] = target;
            plan.count += 1;
            self.window_pending_loads = self.window_pending_loads.saturating_add(1);
            i += 1;
        }
        plan
    }

    fn current_room_request_can_wait(&self, room: RoomIndex) -> bool {
        room == INVALID_ROOM_INDEX
            || self.is_resident(room)
            || self.is_loading(room)
            || !self.job.is_active()
    }

    fn abort_active_load(&mut self) {
        if !self.job.is_active() {
            return;
        }
        debug_log_stream_plan("stream abort", &self.job_plan);
        self.job.abort();
        let plan = self.job_plan;
        let mut i = 0usize;
        while i < plan.count.min(N).min(STREAMED_ROOM_SLOT_COUNT) {
            let slot = plan.slots[i];
            if slot < N
                && self.slots[slot].state == RoomStreamSlotState::Loading
                && self.slots[slot].room == plan.rooms[i]
            {
                self.set_slot(slot, StreamedRoomSlot::EMPTY);
            }
            i += 1;
        }
        self.job_plan = RoomStreamLoadPlan::EMPTY;
    }

    fn start_load_plan(&mut self, plan: RoomStreamLoadPlan<N>) {
        if plan.count == 0 || self.job.is_active() {
            return;
        }
        debug_log_stream_plan("stream start", &plan);
        let mut room_ids = [u16::MAX; N];
        let mut i = 0usize;
        while i < plan.count.min(N) {
            room_ids[i] = plan.rooms[i].raw();
            i += 1;
        }
        self.job.start::<STREAMED_ROOM_SLOT_BYTES>(
            WORLD_PACK_START_LBA,
            WORLD_PACK_TOC,
            &room_ids[..plan.count],
            &plan.slots[..plan.count],
        );
        self.job_plan = plan;
        if self.job.is_done() {
            self.commit_completed_job();
        }
    }

    fn pump(&mut self, dst: &mut [[u32; STREAMED_ROOM_SLOT_WORDS]; N], max_sectors: usize) -> bool {
        if !self.job.is_active() {
            return false;
        }
        self.job
            .poll_words::<STREAMED_ROOM_SLOT_WORDS>(dst, max_sectors);
        let committed = self.commit_ready_job_entries();
        if self.job.is_done() {
            self.commit_completed_job();
            true
        } else {
            committed
        }
    }

    fn commit_ready_job_entries(&mut self) -> bool {
        let completed = self.job.completed_entries();
        let byte_counts = *self.job.byte_counts();
        let plan = self.job_plan;
        let mut committed = false;
        let mut i = 0usize;
        while i < plan.count.min(N).min(STREAMED_ROOM_SLOT_COUNT) {
            if !completed[i] {
                i += 1;
                continue;
            }
            let target = plan.slots[i];
            if target < N
                && self.slots[target].state == RoomStreamSlotState::Loading
                && self.slots[target].room == plan.rooms[i]
            {
                self.set_slot(
                    target,
                    StreamedRoomSlot {
                        room: plan.rooms[i],
                        byte_count: byte_counts[i],
                        last_used: self.epoch,
                        state: RoomStreamSlotState::Resident,
                    },
                );
                committed = true;
            }
            i += 1;
        }
        committed
    }

    fn commit_completed_job(&mut self) {
        let byte_counts = *self.job.byte_counts();
        let statuses = *self.job.statuses();
        let plan = self.job_plan;
        self.commit_window_loads(&plan, &byte_counts, &statuses);
        self.job = cd_stream::WorldRoomSlotsReadJob::new();
        self.job_plan = RoomStreamLoadPlan::EMPTY;
    }

    fn commit_window_loads(
        &mut self,
        plan: &RoomStreamLoadPlan<N>,
        byte_counts: &[usize; N],
        statuses: &[u32; N],
    ) {
        let mut loaded = 0usize;
        while loaded < plan.count.min(N).min(STREAMED_ROOM_SLOT_COUNT) {
            let target = plan.slots[loaded];
            if target >= N {
                loaded += 1;
                continue;
            }
            if statuses[loaded] == cd_stream::ROOM_CHUNK_STATUS_OK && byte_counts[loaded] > 0 {
                self.set_slot(
                    target,
                    StreamedRoomSlot {
                        room: plan.rooms[loaded],
                        byte_count: byte_counts[loaded],
                        last_used: self.epoch,
                        state: RoomStreamSlotState::Resident,
                    },
                );
                debug_log_stream_entry(
                    "stream loaded",
                    plan.rooms[loaded],
                    target,
                    byte_counts[loaded],
                    statuses[loaded],
                );
            } else {
                self.set_slot(
                    target,
                    StreamedRoomSlot {
                        room: plan.rooms[loaded],
                        byte_count: 0,
                        last_used: self.epoch,
                        state: RoomStreamSlotState::Failed,
                    },
                );
                self.window_failed_loads = self.window_failed_loads.saturating_add(1);
                debug_log_stream_entry(
                    "stream failed",
                    plan.rooms[loaded],
                    target,
                    byte_counts[loaded],
                    statuses[loaded],
                );
            }
            loaded += 1;
        }
    }

    fn emit_counters(&self) {
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_REQUESTS,
            self.window_requests as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_MISSES,
            self.window_misses as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PREFETCH_REQUESTS,
            self.window_prefetch_requests as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_RESIDENT_SLOTS,
            self.resident_slot_count() as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_SLOT_LIMIT,
            self.effective_slot_limit() as u32,
        );
        emit_room_chunk_mask(
            telemetry::counter::ROOM_STREAM_LOADING_MASK_LO,
            telemetry::counter::ROOM_STREAM_LOADING_MASK_HI,
            self.loading_room_mask(),
        );
        emit_room_chunk_mask(
            telemetry::counter::ROOM_STREAM_RESIDENT_MASK_LO,
            telemetry::counter::ROOM_STREAM_RESIDENT_MASK_HI,
            self.resident_room_mask(),
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_EVICTIONS,
            self.window_evictions as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_FAILED_LOADS,
            self.window_failed_loads as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PENDING_LOADS,
            self.window_pending_loads as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PROTECTED_FULL,
            self.window_protected_full as u32,
        );
    }

    fn resident_slot_count(&self) -> usize {
        let mut count = 0usize;
        let mut slot = 0usize;
        let limit = self.effective_slot_limit();
        while slot < limit {
            if self.slots[slot].state == RoomStreamSlotState::Resident {
                count += 1;
            }
            slot += 1;
        }
        count
    }

    fn resident_room_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut slot = 0usize;
        let limit = self.effective_slot_limit();
        while slot < limit {
            let meta = self.slots[slot];
            if meta.state == RoomStreamSlotState::Resident {
                mask.insert_room(meta.room);
            }
            slot += 1;
        }
        mask
    }

    fn loading_room_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut slot = 0usize;
        let limit = self.effective_slot_limit();
        while slot < limit {
            let meta = self.slots[slot];
            if meta.state == RoomStreamSlotState::Loading {
                mask.insert_room(meta.room);
            }
            slot += 1;
        }
        mask
    }

    fn choose_slot(
        &self,
        requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        requested_count: usize,
        reserved_slots: &[usize; N],
        reserved_count: usize,
        allow_eviction: bool,
    ) -> Option<usize> {
        let mut slot = 0usize;
        let slot_limit = self.effective_slot_limit();
        while slot < slot_limit {
            let state = self.slots[slot].state;
            if (state == RoomStreamSlotState::Empty || state == RoomStreamSlotState::Failed)
                && !streamed_slot_reserved(slot, reserved_slots, reserved_count)
            {
                return Some(slot);
            }
            slot += 1;
        }
        if !allow_eviction {
            return None;
        }

        let mut best_slot = None;
        let mut best_age = u32::MAX;
        let mut candidate = 0usize;
        while candidate < slot_limit {
            let meta = self.slots[candidate];
            if meta.state != RoomStreamSlotState::Resident
                || streamed_slot_reserved(candidate, reserved_slots, reserved_count)
                || room_requested(meta.room, requested_rooms, requested_count)
                || self.is_room_pinned(meta.room)
            {
                candidate += 1;
                continue;
            }
            if best_slot.is_none() || meta.last_used < best_age {
                best_slot = Some(candidate);
                best_age = meta.last_used;
            }
            candidate += 1;
        }
        best_slot
    }
}

#[derive(Copy, Clone)]
struct ActiveRuntimeRoom {
    index: RoomIndex,
    stream_slot: u16,
    render_room: Option<RuntimeRoom<'static>>,
    collision_room: RuntimeCollisionRoom<'static>,
    width: u16,
    depth: u16,
    sector_size: i32,
    ambient_rgb: [u8; 3],
    materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
    material_count: usize,
    /// Offset from the current chunk's origin to this chunk's
    /// origin, in engine units.
    offset_x: i32,
    offset_z: i32,
    surface_cache: ActiveRoomSurfaceCache,
}

impl ActiveRuntimeRoom {
    fn render(&self) -> Option<RoomRender<'static, '_>> {
        self.render_room.as_ref().map(|room| room.render())
    }

    fn with_current_room_offsets(
        mut self,
        record: &LevelRoomRecord,
        current_record: &LevelRoomRecord,
    ) -> Self {
        self.offset_x = room_origin_x(record).saturating_sub(room_origin_x(current_record));
        self.offset_z = room_origin_z(record).saturating_sub(room_origin_z(current_record));
        self
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
#[derive(Copy, Clone)]
struct ActiveVisibleCellCache {
    room: RoomIndex,
    anchor_x: i32,
    anchor_z: i32,
    view_sin_key: i16,
    view_cos_key: i16,
    camera_independent: bool,
    rejected_global: u16,
    first: u16,
    count: u16,
    ready: bool,
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
impl ActiveVisibleCellCache {
    const EMPTY: Self = Self {
        room: RoomIndex::ZERO,
        anchor_x: 0,
        anchor_z: 0,
        view_sin_key: 0,
        view_cos_key: 0,
        camera_independent: false,
        rejected_global: 0,
        first: 0,
        count: 0,
        ready: false,
    };
}

#[derive(Copy, Clone)]
struct ActiveRoomWindowJob {
    active: bool,
    update_streaming: bool,
    current_room: RoomIndex,
    requested_rooms: [RoomIndex; MAX_ACTIVE_ROOMS],
    requested_count: usize,
    cursor: usize,
    next_slot: usize,
    rooms: [Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    previous_rooms: [Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
}

impl ActiveRoomWindowJob {
    const EMPTY: Self = Self {
        active: false,
        update_streaming: false,
        current_room: RoomIndex::ZERO,
        requested_rooms: [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS],
        requested_count: 0,
        cursor: 0,
        next_slot: 0,
        rooms: [const { None }; MAX_ACTIVE_ROOMS],
        previous_rooms: [const { None }; MAX_ACTIVE_ROOMS],
    };
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
    /// Room the cached BFS rings below were computed from. The resident and
    /// visible sets are a pure function of `(current_room, radius, static
    /// graph)`, so they are recomputed only when `room_index` changes and
    /// cached between crossings.
    room_rings_root: RoomIndex,
    /// Streaming ring: rooms within `WORLD_STREAM_RADIUS` portal hops, kept
    /// resident in memory. Drives the residency desired-set.
    stream_ring: [RoomIndex; STREAMED_ROOM_SLOT_COUNT],
    stream_ring_count: usize,
    /// Visibility ring: rooms within `WORLD_VISIBILITY_RADIUS` portal hops whose
    /// surface caches are built. Drives the active-window build job's request.
    visibility_ring: [RoomIndex; MAX_ACTIVE_ROOMS],
    visibility_ring_count: usize,
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
    /// Spawn position retained for orbit-mode targeting.
    spawn: RoomPoint,
    /// Font atlas used for the analog-mode required prompt.
    font: Option<FontAtlas>,
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
            room_rings_root: INVALID_ROOM_INDEX,
            stream_ring: [INVALID_ROOM_INDEX; STREAMED_ROOM_SLOT_COUNT],
            stream_ring_count: 0,
            visibility_ring: [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS],
            visibility_ring_count: 0,
            materials: [room_material_fallback(); MAX_ROOM_MATERIALS],
            material_count: 0,
            motor: CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO),
            character: None,
            anim_state: PlayerAnim::Idle,
            anim_start_tick: SimTick::ZERO,
            anim_lock_until_tick: SimTick::ZERO,
            box_prop_broken: [0; BOX_PROP_BROKEN_WORDS],
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
            spawn: RoomPoint::ZERO,
            font: None,
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
    fn init(&mut self, _ctx: &mut Ctx) {
        self.font = Some(FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT));
        self.shadow_material = upload_shadow_texture();
        self.particle_material = Some(upload_particle_texture());

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
        self.spawn = RoomPoint::new(spawn.x, spawn.y, spawn.z);
        self.character = character;
        self.motor
            .snap_to(self.spawn, Angle::from_q12(spawn.yaw as u16));
        self.room_index = spawn.room;
        self.anim_state = PlayerAnim::Idle;
        self.anim_start_tick = SimTick::ZERO;
        self.anim_lock_until_tick = SimTick::ZERO;
        self.box_prop_broken = [0; BOX_PROP_BROKEN_WORDS];
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
        self.load_active_room_window();
        #[cfg(feature = "cd-stream-bench")]
        self.bootstrap_streamed_room_window();
        #[cfg(feature = "cd-stream-benchmark")]
        cd_stream::run_benchmark();
    }

    fn update(&mut self, ctx: &mut Ctx) {
        self.portal_debug_log_cooldown = self.portal_debug_log_cooldown.saturating_sub(1);
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

        if !ctx.pad.is_analog() {
            self.camera_turning_last_tick = false;
            return;
        }

        if ctx.just_pressed(button::SELECT) {
            self.free_orbit = !self.free_orbit;
        }
        let delta_vblanks = 1u16;
        self.advance_box_prop_break_events(delta_vblanks);
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
            telemetry::stage_begin(telemetry::stage::CAMERA);
            self.render_camera = self.free_orbit_camera();
            telemetry::stage_end(telemetry::stage::CAMERA);
            self.refresh_active_room_window_if_needed();
            return;
        }

        let now = ctx.sim_tick;
        let action_locked = self.anim_lock_until_tick > now;
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
    }

    fn render(&mut self, ctx: &mut Ctx) {
        if !ctx.pad.is_analog() {
            if let Some(font) = self.font.as_ref() {
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
                let materials = &active.materials[..active.material_count];
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
                draw_box_props(
                    BOX_PROPS,
                    &self.box_prop_broken,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                draw_box_prop_floor_debris(
                    BOX_PROPS,
                    &self.box_prop_broken,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                draw_box_prop_break_events(
                    &self.box_prop_break_events,
                    BOX_PROPS,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                draw_image_props(
                    IMAGE_PROPS,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
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
            draw_player_hud(
                UI_NODES,
                self.font.as_ref(),
                self.motor.stamina_q12(),
                self.motor_config().stamina_max_q12,
            );
        }
    }
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

    fn player_clip_duration_vblanks(
        &self,
        character: RuntimeCharacter,
        clip: ModelClipIndex,
        video_hz: VideoHz,
    ) -> Option<u32> {
        let runtime_model = self
            .models
            .get(character.model.to_usize())
            .copied()
            .flatten()?;
        let animation = runtime_model.clip(&self.clips, clip)?;
        let sample_rate = animation.sample_rate_hz().max(1) as u32;
        let frames = animation.frame_count().max(1) as u32;
        Some(
            frames
                .saturating_mul(video_hz.as_nonzero_u32())
                .div_ceil(sample_rate),
        )
    }

    fn load_runtime_models(&mut self) {
        let mut i = 0;
        while i < MAX_RUNTIME_MODELS {
            self.models[i] = None;
            i += 1;
        }
        i = 0;
        while i < MAX_RUNTIME_MODEL_CLIPS {
            self.clips[i] = None;
            i += 1;
        }
        self.model_face_count = 0;
        self.model_part_count = 0;
        self.model_vertex_count = 0;

        for (index, clip) in MODEL_CLIPS.iter().enumerate() {
            if index >= MAX_RUNTIME_MODEL_CLIPS {
                break;
            }
            let Some(asset) =
                find_asset_of_kind(ASSETS, clip.animation_asset, AssetKind::ModelAnimation)
            else {
                continue;
            };
            self.clips[index] = Animation::from_bytes(asset.bytes).ok();
        }

        for (index, record) in MODELS.iter().enumerate() {
            if index >= MAX_RUNTIME_MODELS {
                break;
            }
            self.models[index] = RuntimeModelAsset::from_record(
                ModelIndex(index as u16),
                record,
                &mut self.model_faces,
                &mut self.model_face_count,
                &mut self.model_parts,
                &mut self.model_part_count,
                &mut self.model_vertices,
                &mut self.model_vertex_count,
            );
        }
    }

    fn motor_config(&self) -> CharacterMotorConfig {
        match self.character {
            Some(c) => c.motor_config(),
            None => CharacterMotorConfig::character(
                0,
                scaled_player_speed(FALLBACK_PLAYER_SPEED),
                scaled_player_speed(FALLBACK_PLAYER_SPEED),
                FALLBACK_PLAYER_YAW_STEP,
            ),
        }
    }

    fn is_box_prop_broken(&self, index: usize) -> bool {
        let Some((word, mask)) = box_prop_state_bit(index) else {
            return false;
        };
        self.box_prop_broken[word] & mask != 0
    }

    fn mark_box_prop_broken(&mut self, index: usize, impulse_x_q8: i16, impulse_z_q8: i16) -> bool {
        let Some((word, mask)) = box_prop_state_bit(index) else {
            return false;
        };
        if self.box_prop_broken[word] & mask != 0 {
            return false;
        }
        self.box_prop_broken[word] |= mask;
        self.spawn_box_prop_break_event(index, impulse_x_q8, impulse_z_q8);
        true
    }

    fn spawn_box_prop_break_event(&mut self, index: usize, impulse_x_q8: i16, impulse_z_q8: i16) {
        let prop_index = index.min(u16::MAX as usize) as u16;
        let replacement = BoxPropBreakEvent {
            prop_index,
            age: 0,
            impulse_x_q8,
            impulse_z_q8,
        };
        let mut target = 0usize;
        let mut oldest_age = 0u8;
        for (slot, event) in self.box_prop_break_events.iter().enumerate() {
            if !event.is_active() {
                self.box_prop_break_events[slot] = replacement;
                return;
            }
            if event.age >= oldest_age {
                oldest_age = event.age;
                target = slot;
            }
        }
        self.box_prop_break_events[target] = replacement;
    }

    fn advance_box_prop_break_events(&mut self, delta_vblanks: u16) {
        let step = delta_vblanks.max(1).min(u8::MAX as u16) as u8;
        for event in &mut self.box_prop_break_events {
            if !event.is_active() {
                continue;
            }
            event.age = event.age.saturating_add(step);
            if event.age >= BOX_PROP_BREAK_FRAMES {
                *event = BoxPropBreakEvent::EMPTY;
            }
        }
    }

    fn break_box_props_for_movement(
        &mut self,
        trigger: u16,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
    ) {
        let current = self.motor.position();
        let target = box_prop_movement_probe_target(
            current,
            self.motor.yaw(),
            input,
            config,
            trigger,
            delta_vblanks,
        );
        for (index, prop) in BOX_PROPS.iter().enumerate() {
            if prop.room != self.room_index
                || prop.flags & trigger == 0
                || self.is_box_prop_broken(index)
            {
                continue;
            }
            let (min, max) = box_prop_aabb(prop);
            if character_body_overlaps_aabb(current, config.radius, config.height, min, max)
                || character_body_overlaps_aabb(target, config.radius, config.height, min, max)
            {
                let (impulse_x_q8, impulse_z_q8) = box_prop_break_impulse_from_delta(
                    target.x.saturating_sub(current.x),
                    target.z.saturating_sub(current.z),
                );
                self.mark_box_prop_broken(index, impulse_x_q8, impulse_z_q8);
            }
        }
    }

    fn break_box_props_for_attack(&mut self, config: CharacterMotorConfig) {
        let origin = self.motor.position();
        let yaw = self.motor.yaw();
        for (index, prop) in BOX_PROPS.iter().enumerate() {
            if prop.room != self.room_index
                || prop.flags & box_prop_flags::BREAK_ON_ATTACK == 0
                || self.is_box_prop_broken(index)
            {
                continue;
            }
            let (min, max) = box_prop_aabb(prop);
            if box_prop_intersects_attack_volume(origin, yaw, config, min, max) {
                let center_x = min.x.saturating_add(max.x) / 2;
                let center_z = min.z.saturating_add(max.z) / 2;
                let mut impulse = box_prop_break_impulse_from_delta(
                    center_x.saturating_sub(origin.x),
                    center_z.saturating_sub(origin.z),
                );
                if impulse == (0, 0) {
                    impulse = box_prop_break_impulse_from_yaw(yaw);
                }
                self.mark_box_prop_broken(index, impulse.0, impulse.1);
            }
        }
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

    fn collect_box_prop_collision_blockers(
        &self,
        out: &mut [CharacterCollisionAabb; MAX_BOX_PROP_BLOCKERS],
    ) -> usize {
        let mut count = 0usize;
        for (index, prop) in BOX_PROPS.iter().enumerate() {
            if prop.room != self.room_index
                || prop.flags & box_prop_flags::COLLISION_ENABLED == 0
                || self.is_box_prop_broken(index)
                || count >= out.len()
            {
                continue;
            }
            let (min, max) = box_prop_aabb(prop);
            out[count] = CharacterCollisionAabb::new(min, max);
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
            );
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
            );
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
            );
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

    fn chunked_level(&self) -> bool {
        !ROOM_CHUNKS.is_empty()
    }

    fn active_room_selection_view(&self) -> ActiveRoomView {
        ActiveRoomView::from_camera(self.render_camera)
    }

    fn rebuild_portal_visibility(
        &mut self,
        current_index: RoomIndex,
        current_record: &LevelRoomRecord,
        view: ActiveRoomView,
        camera_global: RoomPoint,
    ) {
        let half_fov_x_tan_q12 = ((SCREEN_CX as i32).saturating_mul(4096) / FOCAL.max(1)).max(1);
        let half_fov_y_tan_q12 = ((SCREEN_CY as i32).saturating_mul(4096) / FOCAL.max(1)).max(1);
        let far_z = current_record.draw_distance.clamp(NEAR_Z, FAR_Z);
        self.portal_visibility_root = current_index;
        self.portal_visibility_camera_global = camera_global;
        telemetry::stage_begin(telemetry::stage::PORTAL_VISIBILITY);
        let camera = PortalVisibilityCamera::new(
            camera_global.x,
            camera_global.y,
            camera_global.z,
            view.sin_yaw,
            view.cos_yaw,
            view.sin_pitch,
            view.cos_pitch,
            PROJECTION.near_z,
            far_z,
            half_fov_x_tan_q12,
            half_fov_y_tan_q12,
            RUNTIME_SCHEDULE.portal_min_width_q12,
        );
        // The room bounds are a pure function of the static cooked geometry, so
        // collect them once and reuse the cached length on every later refresh.
        let bounds_count = match self.portal_room_bounds_count {
            Some(count) => count,
            None => {
                let count = collect_portal_room_bounds(&mut self.portal_room_bounds);
                self.portal_room_bounds_count = Some(count);
                count
            }
        };
        build_portal_visibility_with_room_bounds(
            ROOMS,
            ROOM_PORTALS,
            &self.portal_room_bounds[..bounds_count],
            current_index,
            camera,
            RUNTIME_SCHEDULE.portal_max_depth,
            &mut self.portal_visibility,
        );
        telemetry::stage_end(telemetry::stage::PORTAL_VISIBILITY);
        if PORTAL_VIS_DEBUG_LOGS
            && self.portal_debug_log_cooldown == 0
            && should_debug_log_portal_visibility(current_record, &self.portal_visibility)
        {
            let player_local = self.motor.position();
            let player_global = local_to_global_room_point(self.room_index, player_local);
            debug_log_portal_visibility_snapshot(
                current_index,
                current_record,
                self.room_index,
                player_local,
                player_global,
                view,
                camera,
                &self.portal_visibility,
            );
            self.portal_debug_log_cooldown = PORTAL_VIS_DEBUG_LOG_COOLDOWN_TICKS;
        }
    }

    fn refresh_portal_visibility_for_view(
        &mut self,
        current_index: RoomIndex,
        current_record: &LevelRoomRecord,
        view: ActiveRoomView,
    ) {
        let visibility_space = portal_visibility_space_for_view(current_index, view);
        let visibility_index = visibility_space.room;
        let visibility_record = ROOMS
            .get(visibility_index.to_usize())
            .unwrap_or(current_record);
        let (view_sin_key, view_cos_key, view_pitch_sin_key, view_pitch_cos_key) =
            portal_visibility_view_keys(view);
        self.active_room_view_sin_key = view_sin_key;
        self.active_room_view_cos_key = view_cos_key;
        self.active_room_view_pitch_sin_key = view_pitch_sin_key;
        self.active_room_view_pitch_cos_key = view_pitch_cos_key;
        self.active_room_view_anchor = view.position;
        self.rebuild_portal_visibility(
            visibility_index,
            visibility_record,
            visibility_space.view,
            visibility_space.camera_global,
        );
        self.active_room_candidates = self.portal_visibility.stats.portals_tested.min(u16::MAX);
        self.portal_visible_missing_resident = 0;
        self.portal_visible_missing_mask = RuntimeDebugMask::EMPTY;
        self.portal_visible_build_failed = 0;
        self.portal_visible_build_failed_mask = RuntimeDebugMask::EMPTY;
    }

    fn portal_visible_room_limit(&self, current_record: &LevelRoomRecord) -> usize {
        self.portal_visibility
            .room_count
            .min(room_active_chunk_limit(current_record))
            .min(MAX_ACTIVE_ROOMS)
    }

    fn portal_visible_rooms_are_active(&self, current_record: &LevelRoomRecord) -> bool {
        let visible_limit = self.portal_visible_room_limit(current_record);
        let mut i = 0usize;
        while i < visible_limit {
            if !self.active_room_contains_drawable(self.portal_visibility.rooms[i].room) {
                return false;
            }
            i += 1;
        }
        true
    }

    fn active_room_contains_drawable(&self, index: RoomIndex) -> bool {
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.active_rooms[slot] {
                if active.index == index
                    && (index == self.room_index
                        || active.render_room.is_some()
                        || active.surface_cache.ready)
                {
                    return true;
                }
            }
            slot += 1;
        }
        false
    }

    fn retain_previous_active_rooms(
        &mut self,
        previous_active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
        current_record: &LevelRoomRecord,
        active_limit: usize,
        next_slot: &mut usize,
    ) {
        let retained_limit = next_slot
            .saturating_add(RUNTIME_SCHEDULE.retained_inactive_rooms)
            .min(active_limit)
            .min(MAX_ACTIVE_ROOMS);
        let mut previous_slot = 0usize;
        while *next_slot < retained_limit && previous_slot < MAX_ACTIVE_ROOMS {
            let Some(previous) = previous_active_rooms[previous_slot] else {
                previous_slot += 1;
                continue;
            };
            previous_slot += 1;
            if previous.stream_slot != active_room_stream_slot(previous.index)
                || self.active_room_contains(previous.index)
            {
                continue;
            }
            let Some(record) = ROOMS.get(previous.index.to_usize()) else {
                continue;
            };
            self.active_rooms[*next_slot] =
                Some(previous.with_current_room_offsets(record, current_record));
            *next_slot += 1;
        }
    }

    fn active_room_contains(&self, index: RoomIndex) -> bool {
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if self.active_rooms[slot].is_some_and(|active| active.index == index) {
                return true;
            }
            slot += 1;
        }
        false
    }

    fn active_room_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.active_rooms[slot] {
                mask.insert_room(active.index);
            }
            slot += 1;
        }
        mask
    }

    fn active_room_drawable_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.active_rooms[slot] {
                if active.index == self.room_index
                    || active.render_room.is_some()
                    || active.surface_cache.ready
                {
                    mask.insert_room(active.index);
                }
            }
            slot += 1;
        }
        mask
    }

    fn portal_visibility_draws_room(&self, index: RoomIndex) -> bool {
        portal_visibility_result_draws_room(&self.portal_visibility, self.room_index, index)
    }

    fn emit_portal_visibility_counters(&self) {
        let stats = self.portal_visibility.stats;
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_CURRENT_ROOM,
            self.portal_visibility_root.raw() as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_VISIBLE_ROOMS,
            self.portal_visibility.room_count as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_FRONTIER_ROOMS,
            self.portal_visibility.frontier_count as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_FRUSTUMS,
            self.portal_visibility.frustum_count as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_PORTALS_TESTED,
            stats.portals_tested as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_PORTALS_ACCEPTED,
            stats.portals_accepted as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_REJECT_BACKFACE,
            stats.reject_backface as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM,
            stats.reject_frustum as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_REJECT_TINY,
            stats.reject_tiny as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACKS,
            stats.bounds_fallbacks as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_CAP_ROOM,
            stats.cap_room as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_CAP_FRUSTUM,
            stats.cap_frustum as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_CAP_DEPTH,
            stats.cap_depth as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_VISIBLE_MISSING_RESIDENT,
            self.portal_visible_missing_resident as u32,
        );
        telemetry::counter(
            telemetry::counter::PORTAL_VIS_VISIBLE_BUILD_FAILED,
            self.portal_visible_build_failed as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PRIORITY_CURRENT,
            self.portal_stream_priority_current as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PRIORITY_VISIBLE,
            self.portal_stream_priority_visible as u32,
        );
        telemetry::counter(
            telemetry::counter::ROOM_STREAM_PRIORITY_FRONTIER,
            self.portal_stream_priority_frontier as u32,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_VISIBLE_MASK_LO,
            telemetry::counter::PORTAL_VIS_VISIBLE_MASK_HI,
            self.portal_visibility.visible_room_mask(),
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_FRONTIER_MASK_LO,
            telemetry::counter::PORTAL_VIS_FRONTIER_MASK_HI,
            self.portal_visibility.frontier_room_mask(),
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_MISSING_MASK_LO,
            telemetry::counter::PORTAL_VIS_MISSING_MASK_HI,
            self.portal_visible_missing_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_BUILD_FAILED_MASK_LO,
            telemetry::counter::PORTAL_VIS_BUILD_FAILED_MASK_HI,
            self.portal_visible_build_failed_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_TESTED_MASK_LO,
            telemetry::counter::PORTAL_VIS_TESTED_MASK_HI,
            stats.tested_room_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_ACCEPTED_MASK_LO,
            telemetry::counter::PORTAL_VIS_ACCEPTED_MASK_HI,
            stats.accepted_room_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_LO,
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_HI,
            stats.reject_frustum_room_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_LO,
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_HI,
            stats.bounds_fallback_room_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_TESTED_PORTAL_MASK_LO,
            telemetry::counter::PORTAL_VIS_TESTED_PORTAL_MASK_HI,
            stats.tested_portal_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_LO,
            telemetry::counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_HI,
            stats.accepted_portal_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_LO,
            telemetry::counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_HI,
            stats.reject_frustum_portal_mask,
        );
        emit_room_chunk_mask(
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_LO,
            telemetry::counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_HI,
            stats.bounds_fallback_portal_mask,
        );
    }

    fn load_active_room_window(&mut self) {
        self.active_room_job = ActiveRoomWindowJob::EMPTY;
        if !self.chunked_level() {
            self.rebuild_active_room_window(true);
            return;
        }
        self.rebase_active_rooms_to_current_room();
        #[cfg(all(
            feature = "world-grid-visible",
            not(feature = "vis-full-active-chunks")
        ))]
        {
            self.clear_visible_cell_caches();
        }
        self.apply_current_active_room_fields();
        self.begin_active_room_window_job(true);
        if self.current_collision_room.is_none() {
            self.step_active_room_window_job();
        }
    }

    fn rebase_active_rooms_to_current_room(&mut self) {
        let Some(current_record) = ROOMS.get(self.room_index.to_usize()) else {
            return;
        };
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            let Some(active) = self.active_rooms[slot] else {
                slot += 1;
                continue;
            };
            let Some(record) = ROOMS.get(active.index.to_usize()) else {
                self.active_rooms[slot] = None;
                slot += 1;
                continue;
            };
            if active.stream_slot != active_room_stream_slot(active.index) {
                self.active_rooms[slot] = None;
                slot += 1;
                continue;
            }
            self.active_rooms[slot] =
                Some(active.with_current_room_offsets(record, current_record));
            slot += 1;
        }
    }

    /// Recompute the cached streaming + visibility BFS rings if the current
    /// room changed since they were last built. The rings are a pure function
    /// of `(current_room, radius, static graph)`, so they are cached between
    /// crossings and only rebuilt when `room_index` moves.
    fn ensure_room_rings(&mut self) {
        if self.room_rings_root == self.room_index {
            return;
        }
        let mut s = [INVALID_ROOM_INDEX; STREAMED_ROOM_SLOT_COUNT];
        let sc = room_graph_ring(
            self.room_index,
            WORLD_STREAM_RADIUS,
            &mut s,
            STREAMED_ROOM_SLOT_COUNT,
        );
        let mut v = [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS];
        let vc = room_graph_ring(
            self.room_index,
            WORLD_VISIBILITY_RADIUS,
            &mut v,
            MAX_ACTIVE_ROOMS,
        );
        self.stream_ring = s;
        self.stream_ring_count = sc;
        self.visibility_ring = v;
        self.visibility_ring_count = vc;
        self.room_rings_root = self.room_index;
    }

    fn begin_active_room_window_job(&mut self, update_streaming: bool) {
        if !self.chunked_level() {
            return;
        }
        let current_index = self.room_index;
        let Some(current_record) = ROOMS.get(current_index.to_usize()) else {
            return;
        };
        let view = self.active_room_selection_view();
        self.refresh_portal_visibility_for_view(current_index, current_record, view);

        let mut requested_rooms = [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS];
        let mut requested_count = self.portal_visible_room_limit(current_record);
        if requested_count == 0 {
            requested_rooms[0] = current_index;
            requested_count = 1;
        } else {
            let mut i = 0usize;
            while i < requested_count {
                requested_rooms[i] = self.portal_visibility.rooms[i].room;
                i += 1;
            }
        }

        self.active_room_anchor = self.motor.position();
        self.active_room_cache_skips = 0;
        self.active_room_job = ActiveRoomWindowJob {
            active: true,
            update_streaming,
            current_room: current_index,
            requested_rooms,
            requested_count,
            cursor: 0,
            next_slot: 0,
            rooms: [const { None }; MAX_ACTIVE_ROOMS],
            previous_rooms: self.active_rooms,
        };
        telemetry::counter(telemetry::counter::ROOM_WINDOW_REBUILDS, 1);
    }

    fn step_active_room_window_job(&mut self) {
        if !self.active_room_job.active {
            return;
        }
        let current_room = self.active_room_job.current_room;
        if current_room != self.room_index {
            self.active_room_job = ActiveRoomWindowJob::EMPTY;
            return;
        }
        let Some(current_record) = ROOMS.get(current_room.to_usize()) else {
            self.active_room_job = ActiveRoomWindowJob::EMPTY;
            return;
        };

        // Residency is owned by update_room_residency now; the build job no
        // longer requests streaming itself, it only builds from resident rooms.

        telemetry::stage_begin(telemetry::stage::ACTIVE_ROOM_WINDOW);
        let mut built_this_tick = 0usize;
        let mut skipped = 0u16;
        let mut unbuilt_room = INVALID_ROOM_INDEX;
        let mut current_active = None;
        {
            let job = &mut self.active_room_job;
            while job.cursor < job.requested_count
                && job.next_slot < MAX_ACTIVE_ROOMS
                && built_this_tick < RUNTIME_SCHEDULE.active_job_builds_per_tick
            {
                let index = job.requested_rooms[job.cursor];
                if index == INVALID_ROOM_INDEX {
                    job.cursor += 1;
                    continue;
                }
                let Some(record) = ROOMS.get(index.to_usize()) else {
                    job.cursor += 1;
                    continue;
                };
                match reuse_or_build_active_room(
                    job.next_slot,
                    index,
                    record,
                    current_record,
                    &job.previous_rooms,
                ) {
                    Some(active)
                        if job.cursor == 0
                            || active.render_room.is_some()
                            || active.surface_cache.ready =>
                    {
                        job.rooms[job.next_slot] = Some(active);
                        if active.index == current_room {
                            current_active = Some(active);
                        }
                        job.next_slot += 1;
                        job.cursor += 1;
                        built_this_tick += 1;
                    }
                    Some(_) => {
                        skipped = skipped.saturating_add(1);
                        job.cursor += 1;
                    }
                    None => {
                        unbuilt_room = index;
                        #[cfg(feature = "cd-stream-bench")]
                        {
                            if streamed_room_is_loading(index) || !streamed_room_is_resident(index)
                            {
                                break;
                            }
                            job.cursor += 1;
                        }
                        #[cfg(not(feature = "cd-stream-bench"))]
                        {
                            job.cursor += 1;
                        }
                    }
                }
            }
        }
        self.active_room_cache_skips = self.active_room_cache_skips.saturating_add(skipped);
        if unbuilt_room != INVALID_ROOM_INDEX {
            self.mark_visible_room_unbuilt(unbuilt_room);
        }
        if let Some(active) = current_active {
            self.apply_current_active_room(active);
        }

        telemetry::counter(
            telemetry::counter::ROOM_WINDOW_BUILT_CHUNKS,
            built_this_tick as u32,
        );
        telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);

        if self.active_room_job.cursor >= self.active_room_job.requested_count
            || self.active_room_job.next_slot >= MAX_ACTIVE_ROOMS
        {
            self.active_rooms = self.active_room_job.rooms;
            let previous_rooms = self.active_room_job.previous_rooms;
            let mut next_slot = self.active_room_job.next_slot;
            self.retain_previous_active_rooms(
                &previous_rooms,
                current_record,
                room_active_chunk_limit(current_record),
                &mut next_slot,
            );
            self.apply_current_active_room_fields();
            self.active_room_job = ActiveRoomWindowJob::EMPTY;
        }
    }

    fn apply_current_active_room_fields(&mut self) {
        self.room = None;
        self.current_collision_room = None;
        self.current_ambient_rgb = [0x80, 0x80, 0x80];
        self.materials = [room_material_fallback(); MAX_ROOM_MATERIALS];
        self.material_count = 0;
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = self.active_rooms[slot] {
                if active.index == self.room_index {
                    self.apply_current_active_room(active);
                    return;
                }
            }
            slot += 1;
        }
    }

    fn apply_current_active_room(&mut self, active: ActiveRuntimeRoom) {
        self.room = active.render_room;
        self.current_collision_room = Some(active.collision_room);
        self.current_ambient_rgb = active.ambient_rgb;
        self.materials = active.materials;
        self.material_count = active.material_count;
    }

    fn refresh_active_room_materials(&mut self) {
        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            if let Some(mut active) = self.active_rooms[slot] {
                if let Some(record) = ROOMS.get(active.index.to_usize()) {
                    let (materials, material_count) = build_runtime_room_material_table(record);
                    active.materials = materials;
                    active.material_count = material_count;
                    self.active_rooms[slot] = Some(active);
                }
            }
            slot += 1;
        }
        self.apply_current_active_room_fields();
    }

    fn mark_visible_room_unbuilt(&mut self, index: RoomIndex) {
        #[cfg(feature = "cd-stream-bench")]
        {
            if streamed_room_is_resident(index) {
                self.portal_visible_build_failed =
                    self.portal_visible_build_failed.saturating_add(1);
                self.portal_visible_build_failed_mask |= room_index_debug_mask(index);
            } else if !streamed_room_is_loading(index) {
                self.portal_visible_missing_resident =
                    self.portal_visible_missing_resident.saturating_add(1);
                self.portal_visible_missing_mask |= room_index_debug_mask(index);
            }
        }
        #[cfg(not(feature = "cd-stream-bench"))]
        {
            self.portal_visible_build_failed = self.portal_visible_build_failed.saturating_add(1);
            self.portal_visible_build_failed_mask |= room_index_debug_mask(index);
        }
    }

    fn rebuild_active_room_window(&mut self, update_streaming: bool) {
        #[cfg(not(feature = "cd-stream-bench"))]
        let _ = update_streaming;

        telemetry::stage_begin(telemetry::stage::ACTIVE_ROOM_WINDOW);
        telemetry::counter(telemetry::counter::ROOM_WINDOW_REBUILDS, 1);
        let previous_active_rooms = self.active_rooms;
        self.room = None;
        self.current_collision_room = None;
        self.current_ambient_rgb = [0x80, 0x80, 0x80];
        self.materials = [room_material_fallback(); MAX_ROOM_MATERIALS];
        self.material_count = 0;
        self.active_rooms = [const { None }; MAX_ACTIVE_ROOMS];
        self.active_room_candidates = 0;
        self.active_room_cache_skips = 0;
        #[cfg(all(
            feature = "world-grid-visible",
            not(feature = "vis-full-active-chunks")
        ))]
        {
            self.clear_visible_cell_caches();
        }

        let current_index = self.room_index;
        let Some(current_record) = ROOMS.get(current_index.to_usize()) else {
            telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);
            return;
        };
        let player = self.motor.position();
        let view = self.active_room_selection_view();
        let active_limit = room_active_chunk_limit(current_record);
        self.refresh_portal_visibility_for_view(current_index, current_record, view);

        let desired_visible_count = self.portal_visible_room_limit(current_record);
        let mut next_slot = 0usize;
        let mut visible_slot = 0usize;
        self.active_room_anchor = player;

        while visible_slot < desired_visible_count && next_slot < MAX_ACTIVE_ROOMS {
            let index = self.portal_visibility.rooms[visible_slot].room;
            let Some(record) = ROOMS.get(index.to_usize()) else {
                visible_slot += 1;
                continue;
            };
            match reuse_or_build_active_room(
                next_slot,
                index,
                record,
                current_record,
                &previous_active_rooms,
            ) {
                Some(active)
                    if visible_slot == 0
                        || active.render_room.is_some()
                        || active.surface_cache.ready =>
                {
                    if index == current_index {
                        self.room = active.render_room;
                        self.current_collision_room = Some(active.collision_room);
                        self.current_ambient_rgb = active.ambient_rgb;
                        self.materials = active.materials;
                        self.material_count = active.material_count;
                    }
                    self.active_rooms[next_slot] = Some(active);
                    next_slot += 1;
                }
                Some(_) => {
                    self.active_room_cache_skips = self.active_room_cache_skips.saturating_add(1);
                }
                None => {
                    self.mark_visible_room_unbuilt(index);
                    if visible_slot == 0 {
                        break;
                    }
                }
            }
            visible_slot += 1;
        }

        if self.current_collision_room.is_none() && next_slot < MAX_ACTIVE_ROOMS {
            if let Some(active) = reuse_or_build_active_room(
                next_slot,
                current_index,
                current_record,
                current_record,
                &previous_active_rooms,
            ) {
                self.room = active.render_room;
                self.current_collision_room = Some(active.collision_room);
                self.current_ambient_rgb = active.ambient_rgb;
                self.materials = active.materials;
                self.material_count = active.material_count;
                self.active_rooms[next_slot] = Some(active);
                next_slot += 1;
            }
        }

        if next_slot == 0 {
            #[cfg(not(feature = "cd-stream-bench"))]
            {
                if let Some(active) = reuse_or_build_active_room(
                    0,
                    current_index,
                    current_record,
                    current_record,
                    &previous_active_rooms,
                ) {
                    self.room = active.render_room;
                    self.current_collision_room = Some(active.collision_room);
                    self.current_ambient_rgb = active.ambient_rgb;
                    self.materials = active.materials;
                    self.material_count = active.material_count;
                    self.active_rooms[0] = Some(active);
                    next_slot = 1;
                }
            }
        }

        self.retain_previous_active_rooms(
            &previous_active_rooms,
            current_record,
            active_limit,
            &mut next_slot,
        );

        if self.portal_visibility.room_count == 0 {
            let visibility_space = portal_visibility_space_for_view(current_index, view);
            let visibility_record = ROOMS
                .get(visibility_space.room.to_usize())
                .unwrap_or(current_record);
            self.rebuild_portal_visibility(
                visibility_space.room,
                visibility_record,
                visibility_space.view,
                visibility_space.camera_global,
            );
        }
        if self.portal_visibility.room_count == 0 {
            self.portal_visible_missing_resident = 0;
            self.portal_visible_missing_mask = RuntimeDebugMask::EMPTY;
            self.portal_visible_build_failed = 0;
            self.portal_visible_build_failed_mask = RuntimeDebugMask::EMPTY;
        }
        telemetry::counter(
            telemetry::counter::ROOM_WINDOW_BUILT_CHUNKS,
            next_slot as u32,
        );
        #[cfg(feature = "cd-stream-bench")]
        if update_streaming {
            self.preload_streamed_active_room_window(desired_visible_count, current_record);
        }
        telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);
    }

    #[cfg(feature = "cd-stream-bench")]
    fn preload_streamed_active_room_window(
        &mut self,
        desired_visible_count: usize,
        current_record: &LevelRoomRecord,
    ) {
        // Residency is owned by update_room_residency now; this path only
        // builds the active window from whatever the owner made resident.
        let visible_limit = desired_visible_count
            .min(self.portal_visibility.room_count)
            .min(room_active_chunk_limit(current_record));

        let previous_active_rooms = self.active_rooms;
        let mut rebuilt = [const { None }; MAX_ACTIVE_ROOMS];
        let mut next_slot = 0usize;
        let active_limit = room_active_chunk_limit(current_record).min(MAX_ACTIVE_ROOMS);
        let mut visible_slot = 0usize;
        self.portal_visible_missing_resident = 0;
        self.portal_visible_missing_mask = RuntimeDebugMask::EMPTY;
        self.portal_visible_build_failed = 0;
        self.portal_visible_build_failed_mask = RuntimeDebugMask::EMPTY;
        if next_slot < active_limit {
            match reuse_or_build_active_room(
                next_slot,
                self.room_index,
                current_record,
                current_record,
                &previous_active_rooms,
            ) {
                Some(active) => {
                    rebuilt[next_slot] = Some(active);
                    next_slot += 1;
                }
                None => self.mark_visible_room_unbuilt(self.room_index),
            }
        }
        while visible_slot < visible_limit && next_slot < active_limit {
            let index = self.portal_visibility.rooms[visible_slot].room;
            if index == self.room_index {
                visible_slot += 1;
                continue;
            }
            if let Some(record) = ROOMS.get(index.to_usize()) {
                match reuse_or_build_active_room(
                    next_slot,
                    index,
                    record,
                    current_record,
                    &previous_active_rooms,
                ) {
                    Some(active)
                        if visible_slot == 0
                            || active.render_room.is_some()
                            || active.surface_cache.ready =>
                    {
                        rebuilt[next_slot] = Some(active);
                        next_slot += 1;
                    }
                    Some(_) => {
                        self.active_room_cache_skips =
                            self.active_room_cache_skips.saturating_add(1);
                    }
                    None => {
                        self.mark_visible_room_unbuilt(index);
                        if visible_slot == 0 {
                            break;
                        }
                    }
                }
            }
            visible_slot += 1;
        }
        self.active_rooms = rebuilt;
        self.retain_previous_active_rooms(
            &previous_active_rooms,
            current_record,
            active_limit,
            &mut next_slot,
        );
        self.apply_current_active_room_fields();
    }

    #[cfg(feature = "cd-stream-bench")]
    fn pump_room_stream(&mut self, max_sectors: usize) -> bool {
        unsafe { ROOM_STREAM_SCHEDULER.pump(&mut STREAMED_ROOM_WORDS, max_sectors) }
    }

    /// The residency owner: computes the single desired resident set -- the
    /// whole level when it fits the budget, otherwise the current room plus its
    /// visible neighbourhood -- and hands it to the scheduler to pin + load.
    /// This is the one place residency is declared; the build paths read
    /// residency from what this makes resident.
    #[cfg(feature = "cd-stream-bench")]
    fn update_room_residency(&mut self) {
        // Residency desired-set is the streaming BFS ring: every room within
        // WORLD_STREAM_RADIUS portal hops of the current room. Computed once per
        // crossing and cached; recompute here is a no-op unless the room moved.
        self.ensure_room_rings();
        let mut desired = [INVALID_ROOM_INDEX; STREAMED_ROOM_SLOT_COUNT];
        let mut count = self.stream_ring_count.min(STREAMED_ROOM_SLOT_COUNT);
        desired[..count].copy_from_slice(&self.stream_ring[..count]);
        // The BFS ring is prefetch; portal visibility is correctness. Whatever
        // the renderer can currently see MUST be resident even when the graph
        // ring and the geometric visibility disagree, or it draws nothing.
        let visible = self.portal_visibility.room_count.min(MAX_ACTIVE_ROOMS);
        let mut i = 0usize;
        while i < visible && count < STREAMED_ROOM_SLOT_COUNT {
            let room = self.portal_visibility.rooms[i].room;
            if room != INVALID_ROOM_INDEX && !room_requested(room, &desired, count) {
                desired[count] = room;
                count += 1;
            }
            i += 1;
        }
        unsafe { ROOM_STREAM_SCHEDULER.reconcile_residency(&desired, count) };
    }

    #[cfg(feature = "cd-stream-bench")]
    fn bootstrap_streamed_room_window(&mut self) {
        let mut pumps = 0usize;
        while pumps < RUNTIME_SCHEDULE.stream_bootstrap_pump_limit && streamed_room_stream_active()
        {
            if self.pump_room_stream(RUNTIME_SCHEDULE.stream_pump_sectors_per_tick) {
                self.load_active_room_window();
            }
            pumps += 1;
        }
        if self.current_collision_room.is_none() {
            self.load_active_room_window();
        }
    }

    fn current_floor_link_sector(&self) -> Option<psx_engine::SectorCollision> {
        let room = self.current_collision_room.as_ref()?.collision();
        let sector_size = room.sector_size();
        if sector_size <= 0 {
            return None;
        }
        let player = self.motor.position();
        if player.x < 0 || player.z < 0 {
            return None;
        }
        let sx = player.x / sector_size;
        let sz = player.z / sector_size;
        if sx < 0 || sz < 0 || sx >= room.width() as i32 || sz >= room.depth() as i32 {
            return None;
        }
        room.sector(sx as u16, sz as u16)
    }

    fn current_floor_link_switch_target(&self) -> Option<RoomIndex> {
        let sector = self.current_floor_link_sector()?;
        let player_y = self.motor.position().y;

        if let Some(room) = sector.floor_below_room() {
            let crosses = !sector.has_floor()
                || player_y
                    < min_i32x4(sector.floor_heights()).saturating_sub(FLOOR_LINK_CROSS_EPSILON);
            if crosses && self.can_switch_to_floor_link_room(room) {
                return Some(room);
            }
        }

        if let Some(room) = sector.floor_above_room() {
            let crosses = sector.has_ceiling()
                && player_y
                    > max_i32x4(sector.ceiling_heights()).saturating_add(FLOOR_LINK_CROSS_EPSILON);
            if crosses && self.can_switch_to_floor_link_room(room) {
                return Some(room);
            }
        }

        None
    }

    fn can_switch_to_floor_link_room(&self, room: RoomIndex) -> bool {
        if room == self.room_index || room == INVALID_ROOM_INDEX || room.to_usize() >= ROOMS.len() {
            return false;
        }
        #[cfg(feature = "cd-stream-bench")]
        if self.chunked_level() && !streamed_room_is_resident(room) {
            return false;
        }
        true
    }

    fn update_current_room_from_player(&mut self) -> bool {
        if !self.chunked_level() {
            return false;
        }
        let global = local_to_global_room_point(self.room_index, self.motor.position());
        let Some(next_room) = self
            .current_floor_link_switch_target()
            .or_else(|| room_index_containing_global_from(self.room_index, global))
        else {
            return false;
        };
        if next_room == self.room_index {
            return false;
        }
        let previous_room = self.room_index;
        let previous_local = self.motor.position();
        let local = global_to_local_room_point(next_room, global);
        let camera_delta = RoomPoint::new(
            local.x.saturating_sub(previous_local.x),
            local.y.saturating_sub(previous_local.y),
            local.z.saturating_sub(previous_local.z),
        );
        let camera_before = RoomPoint::new(
            self.render_camera.position.x,
            self.render_camera.position.y,
            self.render_camera.position.z,
        );
        self.room_index = next_room;
        self.motor.relocate(local);
        self.camera.relocate_room_space(camera_delta);
        self.render_camera.position = WorldVertex::new(
            self.render_camera.position.x.saturating_add(camera_delta.x),
            self.render_camera.position.y.saturating_add(camera_delta.y),
            self.render_camera.position.z.saturating_add(camera_delta.z),
        );
        self.lock_target = None;
        self.lock_switch_stick_held = false;
        self.soft_lock_target = None;
        let camera_after = RoomPoint::new(
            self.render_camera.position.x,
            self.render_camera.position.y,
            self.render_camera.position.z,
        );
        debug_log_room_transition(
            previous_room,
            next_room,
            previous_local,
            local,
            global,
            camera_before,
            camera_after,
        );
        self.load_active_room_window();
        #[cfg(feature = "cd-stream-bench")]
        let loading_mask = unsafe { ROOM_STREAM_SCHEDULER.loading_room_mask() };
        #[cfg(not(feature = "cd-stream-bench"))]
        let loading_mask = RuntimeDebugMask::EMPTY;
        let stats = self.portal_visibility.stats;
        debug_log_room_window_after_cross(
            next_room,
            self.portal_visibility.room_count,
            self.portal_visibility.frontier_count,
            self.portal_visibility.visible_room_mask(),
            self.active_room_mask(),
            self.active_room_drawable_mask(),
            loading_mask,
            self.portal_visible_missing_mask,
            self.portal_visible_build_failed_mask,
            self.room.is_some(),
            self.current_collision_room.is_some(),
            stats.portals_tested,
            stats.portals_accepted,
        );
        self.post_cross_debug_frames = RUNTIME_SCHEDULE.post_cross_render_debug_frames;
        true
    }

    fn refresh_active_room_window_if_needed(&mut self) {
        if !self.chunked_level() {
            return;
        }
        let Some(record) = ROOMS.get(self.room_index.to_usize()) else {
            return;
        };
        let sector_size = record.sector_size.max(1);
        let threshold = sector_size.saturating_mul(RUNTIME_SCHEDULE.active_refresh_sectors.max(1));
        let view_threshold = sector_size;
        let player = self.motor.position();
        let view = self.active_room_selection_view();
        let (view_sin_key, view_cos_key, view_pitch_sin_key, view_pitch_cos_key) =
            portal_visibility_view_keys(view);
        let moved_far = point_xz_axis_moved_at_least(player, self.active_room_anchor, threshold);
        let camera_moved_far = point_xyz_axis_moved_at_least(
            view.position,
            self.active_room_view_anchor,
            view_threshold,
        );
        let view_changed = view_sin_key != self.active_room_view_sin_key
            || view_cos_key != self.active_room_view_cos_key
            || view_pitch_sin_key != self.active_room_view_pitch_sin_key
            || view_pitch_cos_key != self.active_room_view_pitch_cos_key;
        if moved_far {
            self.begin_active_room_window_job(true);
            return;
        }
        if !camera_moved_far && !view_changed {
            return;
        }
        self.refresh_portal_visibility_for_view(self.room_index, record, view);
        if !self.active_room_job.active && !self.portal_visible_rooms_are_active(record) {
            self.begin_active_room_window_job(true);
        }
    }

    fn force_refresh_active_room_window_view(&mut self) {
        if !self.chunked_level() {
            return;
        }
        let Some(record) = ROOMS.get(self.room_index.to_usize()) else {
            return;
        };
        let view = self.active_room_selection_view();
        self.refresh_portal_visibility_for_view(self.room_index, record, view);
        if !self.active_room_job.active && !self.portal_visible_rooms_are_active(record) {
            self.begin_active_room_window_job(true);
        }
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
            target.y.saturating_add(height / 2),
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

#[derive(Copy, Clone, Debug, Default)]
struct PlayerModelDrawStats {
    stats: TexturedModelRenderStats,
    bounds_tests: u16,
    bounds_culled: u16,
}

fn draw_player(
    character: RuntimeCharacter,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    x: i32,
    y: i32,
    z: i32,
    yaw: Angle,
    anim_action: CharacterAnimationAction,
    clip_local: ModelClipIndex,
    anim_start_tick: SimTick,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> PlayerModelDrawStats {
    let Some(runtime_model) = models.get(character.model.to_usize()).copied().flatten() else {
        return PlayerModelDrawStats::default();
    };

    let Some(anim) = runtime_model.clip(clips, clip_local) else {
        return PlayerModelDrawStats::default();
    };
    // Phase the animation relative to the clip-start tick so
    // state changes don't pop into the middle of a new clip.
    let local_tick = elapsed_tick.saturating_sub(anim_start_tick);
    let phase = animation_phase_at_tick_q12(
        anim,
        local_tick,
        video_hz,
        character.action_loops(anim_action),
    );
    let bounds = model_frame_bounds(runtime_model, clip_local, phase);
    let clip_anchor = model_clip_anchor(runtime_model, clip_local);
    let reference_anchor = model_clip_anchor(runtime_model, character.clip_for(PlayerAnim::Idle));
    let pose_translation = model_pose_anchor_translation(
        anim,
        phase,
        clip_anchor,
        reference_anchor,
        character.action_in_place_override(anim_action),
    );

    let model_rotation = yaw_rotation_matrix(yaw.add_signed_q12(character.visual_yaw));
    let origin = visual_model_origin(
        x,
        y,
        z,
        runtime_model.world_height,
        character.visual_offset,
        character.visual_scale_q8,
        &model_rotation,
    );
    let local_to_world = visual_model_local_to_world(runtime_model, character.visual_scale_q8);
    let bounds_origin =
        model_pose_translated_origin(origin, model_rotation, local_to_world, pose_translation);
    telemetry::stage_begin(telemetry::stage::PLAYER_BOUNDS);
    let visible = match bounds {
        Some(bounds) if MODEL_BOUNDS_CULLING_ENABLED => model_bounds_visible(
            camera,
            options,
            bounds_origin,
            model_rotation,
            bounds,
            character.visual_scale_q8,
        ),
        _ => true,
    };
    telemetry::stage_end(telemetry::stage::PLAYER_BOUNDS);
    if !visible {
        return PlayerModelDrawStats {
            stats: TexturedModelRenderStats::default(),
            bounds_tests: 1,
            bounds_culled: 1,
        };
    }

    let material = lighting.shade_model_material(origin, runtime_model.material);
    let model_options = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::Back)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(MODEL_TEXTURE_SPLIT_MAX_EDGE);

    telemetry::stage_begin(telemetry::stage::PLAYER_DRAW);
    let faces = runtime_model_faces(runtime_model, model_faces);
    let stats = submit_runtime_model_predecoded(
        world,
        triangles,
        runtime_model,
        anim,
        phase,
        *camera,
        origin,
        model_rotation,
        local_to_world,
        pose_translation,
        material,
        model_options,
        faces,
        model_parts,
        model_vertices,
    );
    telemetry::stage_end(telemetry::stage::PLAYER_DRAW);
    PlayerModelDrawStats {
        stats,
        bounds_tests: 1,
        bounds_culled: 0,
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn submit_runtime_model_predecoded(
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    runtime_model: RuntimeModelAsset,
    anim: Animation<'static>,
    phase: u32,
    camera: WorldCamera,
    origin: WorldVertex,
    rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    pose_translation: ModelPoseTranslation,
    material: TextureMaterial,
    options: WorldSurfaceOptions,
    faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
) -> TexturedModelRenderStats {
    let start_cycles = if MODEL_PROFILE_ENABLED {
        telemetry::cycle_counter()
    } else {
        0
    };
    let Some(geometry) = runtime_model_geometry(runtime_model, model_parts, model_vertices) else {
        let mut stats = TexturedModelRenderStats::default();
        stats.vertex_overflow = true;
        return stats;
    };
    let stats = if runtime_model.requires_cpu_blend {
        world.submit_textured_model_predecoded_geometry_faces(
            triangles,
            runtime_model.model,
            anim,
            phase,
            camera,
            origin,
            rotation,
            local_to_world,
            pose_translation,
            unsafe { &mut MODEL_VERTICES },
            unsafe { &mut JOINT_VIEW_TRANSFORMS },
            material,
            options,
            faces,
            geometry,
        )
    } else {
        world.submit_textured_model_primary_joints_predecoded_geometry_faces(
            triangles,
            runtime_model.model,
            anim,
            phase,
            camera,
            origin,
            rotation,
            local_to_world,
            pose_translation,
            unsafe { &mut MODEL_VERTICES },
            unsafe { &mut JOINT_VIEW_TRANSFORMS },
            material,
            options,
            faces,
            geometry,
        )
    };
    if MODEL_PROFILE_ENABLED {
        emit_runtime_model_profile(runtime_model.index, start_cycles);
    }
    stats
}

fn emit_runtime_model_profile(index: ModelIndex, start_cycles: u32) {
    let Some(cycle_counter) = runtime_model_profile_cycle_counter(index) else {
        return;
    };
    let draw_counter = telemetry::counter::MODEL_PROFILE_DRAWS_0.saturating_add(index.raw().min(7));
    telemetry::counter(draw_counter, 1);
    telemetry::counter(
        cycle_counter,
        telemetry::cycle_counter().wrapping_sub(start_cycles),
    );
}

fn runtime_model_profile_cycle_counter(index: ModelIndex) -> Option<u16> {
    match index.raw() {
        0 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_0),
        1 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_1),
        2 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_2),
        3 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_3),
        4 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_4),
        5 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_5),
        6 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_6),
        7 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_7),
        _ => None,
    }
}

#[derive(Copy, Clone, Debug, Default)]
struct EquipmentDrawStats {
    draws: u16,
    active_hitboxes: u16,
    target_hits: u16,
    stats: TexturedModelRenderStats,
}

#[derive(Copy, Clone)]
struct AttachmentPose {
    origin: WorldVertex,
    rotation: Mat3I16,
}

#[allow(clippy::too_many_arguments)]
fn draw_player_equipment(
    current_room: RoomIndex,
    character: RuntimeCharacter,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    x: i32,
    y: i32,
    z: i32,
    yaw: Angle,
    anim_action: CharacterAnimationAction,
    clip_local: ModelClipIndex,
    anim_start_tick: SimTick,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut out = EquipmentDrawStats::default();
    let Some(character_model) = models.get(character.model.to_usize()).copied().flatten() else {
        return out;
    };
    let Some(character_anim) = character_model.clip(clips, clip_local) else {
        return out;
    };
    let local_tick = elapsed_tick.saturating_sub(anim_start_tick);
    let character_phase = animation_phase_at_tick_q12(
        character_anim,
        local_tick,
        video_hz,
        character.action_loops(anim_action),
    );
    let character_anchor = model_clip_anchor(character_model, clip_local);
    let reference_anchor = model_clip_anchor(character_model, character.clip_for(PlayerAnim::Idle));
    let character_pose_translation = model_pose_anchor_translation(
        character_anim,
        character_phase,
        character_anchor,
        reference_anchor,
        character.action_in_place_override(anim_action),
    );
    let character_frame = (character_phase >> 12) as u16;
    let character_model_rotation = yaw_rotation_matrix(yaw.add_signed_q12(character.visual_yaw));
    let character_origin = visual_model_origin(
        x,
        y,
        z,
        character_model.world_height,
        character.visual_offset,
        character.visual_scale_q8,
        &character_model_rotation,
    );
    let character_local_to_world =
        visual_model_local_to_world(character_model, character.visual_scale_q8);

    let mut drawn = 0usize;
    for equipment in EQUIPMENT {
        if equipment.room != current_room
            || equipment.flags & equipment_flags::PLAYER == 0
            || drawn >= MAX_EQUIPMENT_DRAWS
        {
            continue;
        }
        let Some(weapon) = WEAPONS.get(equipment.weapon.to_usize()) else {
            continue;
        };
        let Some(socket) = find_model_socket(character_model, equipment.character_socket)
            .or_else(|| find_model_socket(character_model, weapon.default_character_socket))
        else {
            continue;
        };
        let Some(socket_pose) = attachment_socket_pose(
            character_model,
            character_anim,
            character_phase,
            character_origin,
            character_model_rotation,
            character_local_to_world,
            character_pose_translation,
            socket,
        ) else {
            continue;
        };
        let weapon_rotation = socket_pose
            .rotation
            .mul(&euler_q12_rotation_inverse(weapon.grip_rotation_q12));

        match weapon.model {
            Some(model_index) => {
                let Some(weapon_model) = models.get(model_index.to_usize()).copied().flatten()
                else {
                    continue;
                };
                let grip = scaled_offset(weapon_model.local_to_world, weapon.grip_translation);
                let grip_world = rotate_offset_q12(&weapon_rotation, grip);
                let origin = WorldVertex::new(
                    socket_pose.origin.x.saturating_sub(grip_world[0]),
                    socket_pose.origin.y.saturating_sub(grip_world[1]),
                    socket_pose.origin.z.saturating_sub(grip_world[2]),
                );
                if let Some(anim) = weapon_model.clip(clips, weapon_model.default_clip) {
                    let phase = anim.phase_at_tick_q12(elapsed_tick.as_u32(), video_hz.as_u16());
                    let material = lighting.shade_model_material(origin, weapon_model.material);
                    let model_options = options
                        .with_depth_policy(DepthPolicy::Average)
                        .with_cull_mode(CullMode::Back)
                        .with_material_layer(material)
                        .with_textured_triangle_splitting(true)
                        .with_textured_triangle_max_edge(MODEL_TEXTURE_SPLIT_MAX_EDGE);
                    let faces = runtime_model_faces(weapon_model, model_faces);
                    let stats = submit_runtime_model_predecoded(
                        world,
                        triangles,
                        weapon_model,
                        anim,
                        phase,
                        *camera,
                        origin,
                        weapon_rotation,
                        weapon_model.local_to_world,
                        ModelPoseTranslation::ZERO,
                        material,
                        model_options,
                        faces,
                        model_parts,
                        model_vertices,
                    );
                    accumulate_model_stats(&mut out.stats, stats);
                    if stats.primitive_overflow || stats.command_overflow {
                        out.draws = drawn as u16;
                        return out;
                    }
                    drawn += 1;
                    out.draws = drawn as u16;
                }
            }
            None => {}
        };

        let (active, hits) = evaluate_weapon_hitboxes(
            current_room,
            weapon.hitbox_first.to_usize(),
            weapon.hitbox_count,
            character_frame,
            socket_pose.origin,
            socket_pose.rotation,
        );
        out.active_hitboxes = out.active_hitboxes.saturating_add(active);
        out.target_hits = out.target_hits.saturating_add(hits);
    }
    out
}

fn find_model_socket(
    model: RuntimeModelAsset,
    name: &str,
) -> Option<&'static LevelModelSocketRecord> {
    let first = model.socket_first.to_usize();
    let count = model.socket_count as usize;
    let sockets = MODEL_SOCKETS.get(first..first.saturating_add(count))?;
    sockets.iter().find(|socket| socket.name == name)
}

fn attachment_socket_pose(
    _model: RuntimeModelAsset,
    animation: Animation<'static>,
    phase_q12: u32,
    origin: WorldVertex,
    instance_rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    pose_translation: ModelPoseTranslation,
    socket: &LevelModelSocketRecord,
) -> Option<AttachmentPose> {
    let pose = apply_model_pose_translation(
        animation.pose_looped_q12(phase_q12, socket.joint)?,
        pose_translation,
    );
    let joint = compute_joint_world_transform(pose, instance_rotation, local_to_world, origin);
    Some(compose_socket_pose(
        joint,
        socket.translation,
        socket.rotation_q12,
    ))
}

fn compose_socket_pose(
    joint: JointWorldTransform,
    translation: [i32; 3],
    rotation_q12: [i16; 3],
) -> AttachmentPose {
    let offset = rotate_offset_q12(&joint.rotation, translation);
    let local_rotation = euler_q12_rotation(rotation_q12);
    AttachmentPose {
        origin: WorldVertex::new(
            joint.translation.x.saturating_add(offset[0]),
            joint.translation.y.saturating_add(offset[1]),
            joint.translation.z.saturating_add(offset[2]),
        ),
        rotation: joint.rotation.mul(&local_rotation),
    }
}

fn evaluate_weapon_hitboxes(
    current_room: RoomIndex,
    first: usize,
    count: u16,
    frame: u16,
    origin: WorldVertex,
    rotation: Mat3I16,
) -> (u16, u16) {
    let mut active = 0u16;
    let mut hits = 0u16;
    let Some(hitboxes) = WEAPON_HITBOXES.get(first..first.saturating_add(count as usize)) else {
        return (0, 0);
    };
    for hitbox in hitboxes {
        if frame < hitbox.active_start_frame || frame > hitbox.active_end_frame {
            continue;
        }
        active = active.saturating_add(1);
        for entity in ENTITIES {
            if entity.room != current_room {
                continue;
            }
            if weapon_hit_shape_hits_point(hitbox.shape, origin, rotation, entity.x, entity.z) {
                hits = hits.saturating_add(1);
            }
        }
    }
    (active, hits)
}

fn weapon_hit_shape_hits_point(
    shape: WeaponHitShapeRecord,
    origin: WorldVertex,
    rotation: Mat3I16,
    px: i32,
    pz: i32,
) -> bool {
    match shape {
        WeaponHitShapeRecord::Box {
            center,
            half_extents,
        } => {
            let c = transform_local_point(origin, rotation, center);
            let radius = half_extents[0].max(half_extents[2]) as i32;
            distance_xz_sq(RoomPoint::new(px, 0, pz), RoomPoint::new(c.x, 0, c.z))
                <= square_i32_saturating(radius)
        }
        WeaponHitShapeRecord::Capsule { start, end, radius } => {
            let a = transform_local_point(origin, rotation, start);
            let b = transform_local_point(origin, rotation, end);
            point_segment_xz_distance_sq(px, pz, a.x, a.z, b.x, b.z)
                <= square_i32_saturating(radius as i32)
        }
    }
}

fn transform_local_point(origin: WorldVertex, rotation: Mat3I16, point: [i32; 3]) -> WorldVertex {
    let offset = rotate_offset_q12(&rotation, point);
    WorldVertex::new(
        origin.x.saturating_add(offset[0]),
        origin.y.saturating_add(offset[1]),
        origin.z.saturating_add(offset[2]),
    )
}

fn scaled_offset(scale: LocalToWorldScale, offset: [i32; 3]) -> [i32; 3] {
    [
        scale.apply(offset[0]),
        scale.apply(offset[1]),
        scale.apply(offset[2]),
    ]
}

fn rotate_offset_q12(rotation: &Mat3I16, offset: [i32; 3]) -> [i32; 3] {
    let row = |r: [i16; 3]| -> i32 {
        let x = (r[0] as i32).saturating_mul(offset[0]);
        let y = (r[1] as i32).saturating_mul(offset[1]);
        let z = (r[2] as i32).saturating_mul(offset[2]);
        x.saturating_add(y).saturating_add(z) >> 12
    };
    [row(rotation.m[0]), row(rotation.m[1]), row(rotation.m[2])]
}

fn euler_q12_rotation(rotation_q12: [i16; 3]) -> Mat3I16 {
    let rx = Mat3I16::rotate_x(Angle::from_q12(rotation_q12[0] as u16).rotate_y_arg());
    let ry = Mat3I16::rotate_y(Angle::from_q12(rotation_q12[1] as u16).rotate_y_arg());
    let rz = Mat3I16::rotate_z(Angle::from_q12(rotation_q12[2] as u16).rotate_y_arg());
    rz.mul(&ry).mul(&rx)
}

fn euler_q12_rotation_inverse(rotation_q12: [i16; 3]) -> Mat3I16 {
    let inv_x = (-(rotation_q12[0] as i32)) as u16;
    let inv_y = (-(rotation_q12[1] as i32)) as u16;
    let inv_z = (-(rotation_q12[2] as i32)) as u16;
    let rx = Mat3I16::rotate_x(Angle::from_q12(inv_x).rotate_y_arg());
    let ry = Mat3I16::rotate_y(Angle::from_q12(inv_y).rotate_y_arg());
    let rz = Mat3I16::rotate_z(Angle::from_q12(inv_z).rotate_y_arg());
    rx.mul(&ry).mul(&rz)
}

fn point_segment_xz_distance_sq(px: i32, pz: i32, ax: i32, az: i32, bx: i32, bz: i32) -> i32 {
    let abx = bx.saturating_sub(ax);
    let abz = bz.saturating_sub(az);
    let apx = px.saturating_sub(ax);
    let apz = pz.saturating_sub(az);
    let denom = square_i32_saturating(abx).saturating_add(square_i32_saturating(abz));
    if denom <= 0 {
        return square_i32_saturating(apx).saturating_add(square_i32_saturating(apz));
    }
    let dot = apx
        .saturating_mul(abx)
        .saturating_add(apz.saturating_mul(abz));
    let t_q8 = ratio_q8_i32(dot.clamp(0, denom), denom);
    let cx = ax.saturating_add((abx.saturating_mul(t_q8)) >> 8);
    let cz = az.saturating_add((abz.saturating_mul(t_q8)) >> 8);
    square_i32_saturating(px.saturating_sub(cx))
        .saturating_add(square_i32_saturating(pz.saturating_sub(cz)))
}

fn emit_model_counters(
    stats: TexturedModelRenderStats,
    projected_counter: u16,
    submitted_counter: u16,
    culled_counter: u16,
    dropped_counter: u16,
) {
    telemetry::counter(projected_counter, stats.projected_vertices as u32);
    telemetry::counter(submitted_counter, stats.submitted_triangles as u32);
    telemetry::counter(culled_counter, stats.culled_triangles as u32);
    telemetry::counter(dropped_counter, stats.dropped_triangles as u32);

    let mut overflow = 0u32;
    if stats.vertex_overflow {
        overflow |= 1;
    }
    if stats.primitive_overflow {
        overflow |= 1 << 1;
    }
    if stats.command_overflow {
        overflow |= 1 << 2;
    }
    if overflow != 0 {
        telemetry::counter(telemetry::counter::MODEL_OVERFLOW_FLAGS, overflow);
    }
}

#[cfg(feature = "world-grid-visible")]
fn accumulate_grid_visibility_stats(total: &mut GridVisibilityStats, stats: GridVisibilityStats) {
    total.cells_considered = total
        .cells_considered
        .saturating_add(stats.cells_considered);
    total.cells_drawn = total.cells_drawn.saturating_add(stats.cells_drawn);
    total.cells_frustum_culled = total
        .cells_frustum_culled
        .saturating_add(stats.cells_frustum_culled);
    total.surfaces_considered = total
        .surfaces_considered
        .saturating_add(stats.surfaces_considered);
    total.projected_vertices = total
        .projected_vertices
        .saturating_add(stats.projected_vertices);
}

/// Cooked cyclorama backdrop. The expensive authored sky art is
/// rasterized into a panorama texture by the editor cooker; runtime
/// wraps that texture over a small camera-centred dome so translation
/// is ignored but yaw/pitch still feel like surrounding scenery.
/// OT slot reserved for the sky cyclorama. It is the farthest slot, drawn
/// behind all world geometry (which `WORLD_BAND` caps at `OT_DEPTH - 2`).
const SKY_OT_SLOT: psx_engine::DepthSlot = psx_engine::DepthSlot::new(OT_DEPTH - 1);

fn draw_sky_panorama(
    sky: LevelSkyRecord,
    camera: WorldCamera,
    primitive_packets: &mut PrimitivePacketArena<'_>,
    ot: &mut OtFrame<'_, OT_DEPTH>,
) {
    if sky.flags & sky_flags::ENABLED == 0 {
        return;
    }
    let Some(asset) = find_asset_of_kind(ASSETS, sky.cloud_layer.texture_asset, AssetKind::Texture)
    else {
        return;
    };
    if ensure_sky_panorama_uploaded(asset.id, asset.bytes).is_none() {
        return;
    }

    let mut columns = sky
        .skybox_columns
        .clamp(SKY_CYCLORAMA_COLUMNS_MIN, SKY_CYCLORAMA_COLUMNS_MAX) as usize;
    if columns % 2 != 0 {
        columns += 1;
    }
    let rows = sky_panorama_runtime_rows(sky);
    let horizon_pitch = sky_horizon_pitch_degrees_i32(sky.horizon_percent);
    let top_pitch = (horizon_pitch + 58).min(78);
    let bottom_pitch = (horizon_pitch - 46).max(-72);
    let mut projected_grid: [Option<(i16, i16)>; SKY_CYCLORAMA_GRID_POINTS_MAX] =
        [None; SKY_CYCLORAMA_GRID_POINTS_MAX];

    // Project the whole grid on the GTE: load the camera rotation once, then
    // RTPS each direction (hardware rotate + perspective divide) instead of the
    // per-direction CPU rotate (eight muls) and two divides.
    let sky_projector = SkyDirectionProjector::load(camera);
    // Yaw depends only on column and pitch only on row, so precompute the
    // sin/cos of each once instead of four trig lookups per grid point.
    let mut yaw_sin = [0i32; SKY_CYCLORAMA_COLUMNS_MAX as usize + 1];
    let mut yaw_cos = [0i32; SKY_CYCLORAMA_COLUMNS_MAX as usize + 1];
    for column in 0..=columns {
        let yaw = angle_from_degrees_i32(sky_yaw_degrees_for_column(column, columns));
        yaw_sin[column] = yaw.sin().raw();
        yaw_cos[column] = yaw.cos().raw();
    }
    let mut pitch_sin = [0i32; SKY_PANORAMA_PALETTE_BANDS + 1];
    let mut pitch_cos = [0i32; SKY_PANORAMA_PALETTE_BANDS + 1];
    for row in 0..=rows {
        let pitch =
            angle_from_degrees_i32(sky_lerp_i32(top_pitch, bottom_pitch, row, rows).clamp(-82, 82));
        pitch_sin[row] = pitch.sin().raw();
        pitch_cos[row] = pitch.cos().raw();
    }
    for row in 0..=rows {
        for column in 0..=columns {
            let dir = [
                clamp_i16(-mul_q12_i32(yaw_sin[column], pitch_cos[row])),
                clamp_i16(pitch_sin[row]),
                clamp_i16(-mul_q12_i32(yaw_cos[column], pitch_cos[row])),
            ];
            projected_grid[sky_grid_index(row, column, columns)] = sky_projector
                .project(dir)
                .map(|(sx, sy)| (sx.clamp(-512, 831), sy.clamp(-256, 495)));
        }
    }

    for row in 0..rows {
        let v0 = sky_uv_for_step(row, rows, SKY_PANORAMA_HEIGHT);
        let v1 = sky_uv_for_step(row + 1, rows, SKY_PANORAMA_HEIGHT);
        let clut_word = sky_panorama_clut_word(sky_panorama_clut_band_for_row(row, rows));
        for column in 0..columns {
            let page = sky_panorama_page_for_column(column, columns);
            let material = TextureMaterial::opaque(
                clut_word,
                sky_panorama_tpage_word(page),
                (0x80, 0x80, 0x80),
            )
            .with_raw_texture(true)
            .with_dither(true);
            let u0 = sky_panorama_local_u(
                sky_coord_for_step(column, columns, SKY_PANORAMA_WIDTH),
                page,
            );
            let u1 = sky_panorama_local_u(
                sky_coord_for_step(column + 1, columns, SKY_PANORAMA_WIDTH),
                page,
            );
            let Some(p0) = projected_grid[sky_grid_index(row, column, columns)] else {
                continue;
            };
            let Some(p1) = projected_grid[sky_grid_index(row, column + 1, columns)] else {
                continue;
            };
            let Some(p2) = projected_grid[sky_grid_index(row + 1, column, columns)] else {
                continue;
            };
            let Some(p3) = projected_grid[sky_grid_index(row + 1, column + 1, columns)] else {
                continue;
            };
            let projected = [p0, p1, p2, p3];
            if sky_quad_outside_screen(projected) {
                continue;
            }
            // Same GP0 words as the old immediate `draw_quad_textured_material`,
            // but pushed into the OT background slot so the whole sky DMAs as
            // one chain instead of per-quad FIFO writes + wait_cmd_ready spins.
            let quad = QuadTexturedMaterial::with_material(
                projected,
                [(u0, v0), (u1, v0), (u0, v1), (u1, v1)],
                material,
            );
            if let Some(packet) = primitive_packets.push(quad) {
                ot.add_packet_slot(SKY_OT_SLOT, packet);
            }
        }
    }
}

fn sky_grid_index(row: usize, column: usize, columns: usize) -> usize {
    row.saturating_mul(columns.saturating_add(1))
        .saturating_add(column)
        .min(SKY_CYCLORAMA_GRID_POINTS_MAX - 1)
}

fn sky_quad_outside_screen(points: [(i16, i16); 4]) -> bool {
    let min_x = points.iter().map(|p| p.0).min().unwrap_or(0);
    let max_x = points.iter().map(|p| p.0).max().unwrap_or(0);
    let min_y = points.iter().map(|p| p.1).min().unwrap_or(0);
    let max_y = points.iter().map(|p| p.1).max().unwrap_or(0);
    max_x < 0 || min_x >= SCREEN_W || max_y < 0 || min_y >= SCREEN_H
}

fn angle_from_degrees_i32(degrees: i32) -> Angle {
    Angle::from_q12(((degrees.saturating_mul(4096) / 360) & 0x0fff) as u16)
}

fn sky_horizon_pitch_degrees_i32(horizon_percent: u8) -> i32 {
    let y = 120 - 240 * i32::from(horizon_percent.clamp(5, 95)) / 100;
    y.saturating_mul(57) / FOCAL
}

fn sky_yaw_degrees_for_column(column: usize, columns: usize) -> i32 {
    -180 + (360 * column as i32) / columns.max(1) as i32
}

fn sky_lerp_i32(a: i32, b: i32, index: usize, count: usize) -> i32 {
    let count = count.max(1) as i32;
    a + (b - a) * index as i32 / count
}

fn sky_coord_for_step(step: usize, steps: usize, size: u16) -> u16 {
    if step >= steps {
        return size.saturating_sub(1);
    }
    ((step as u32 * u32::from(size)) / steps.max(1) as u32).min(u32::from(size - 1)) as u16
}

fn sky_uv_for_step(step: usize, steps: usize, size: u16) -> u8 {
    sky_coord_for_step(step, steps, size).min(255) as u8
}

fn sky_panorama_runtime_rows(sky: LevelSkyRecord) -> usize {
    sky.skybox_rows.clamp(1, SKY_PANORAMA_PALETTE_BANDS as u8) as usize
}

fn sky_panorama_clut_band_for_row(row: usize, rows: usize) -> usize {
    let rows = rows.max(1);
    ((row.saturating_mul(2).saturating_add(1)) * SKY_PANORAMA_PALETTE_BANDS / (rows * 2))
        .min(SKY_PANORAMA_PALETTE_BANDS - 1)
}

fn sky_panorama_page_for_column(column: usize, columns: usize) -> usize {
    if column < columns / 2 {
        0
    } else {
        1
    }
}

fn sky_panorama_local_u(global_u: u16, page: usize) -> u8 {
    let page_u = if page == 0 {
        global_u.min(SKY_PANORAMA_PAGE_WIDTH - 1)
    } else {
        global_u
            .saturating_sub(SKY_PANORAMA_PAGE_WIDTH)
            .min(SKY_PANORAMA_PAGE_WIDTH - 1)
    };
    page_u as u8
}

fn sky_panorama_tpage_word(page: usize) -> u16 {
    if page == 0 {
        SKY_PANORAMA_LEFT_TPAGE.uv_tpage_word(0)
    } else {
        SKY_PANORAMA_RIGHT_TPAGE.uv_tpage_word(0)
    }
}

fn sky_panorama_clut_word(band: usize) -> u16 {
    Clut::new(
        SKY_PANORAMA_CLUT_X,
        SKY_PANORAMA_CLUT_Y + band.min(SKY_PANORAMA_PALETTE_BANDS - 1) as u16,
    )
    .uv_clut_word()
}

fn draw_far_vista_ring(
    camera: WorldCamera,
    vista: LevelFarVistaRecord,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    if vista.flags & far_vista_flags::ENABLED == 0 {
        return;
    }
    let segments = vista.segments.clamp(3, 16);
    let radius = vista.radius.max(1_024);
    let y0 = camera.position.y.saturating_add(vista.vertical_offset);
    let y1 = y0.saturating_add(vista.height.max(128));
    let step = 0x1_0000_u32 / segments as u32;
    let base = angle_from_signed_degrees(vista.rotation_degrees);

    for segment in 0..segments {
        let a0 = base.add(Angle::from_raw_q16(segment as u16 * step as u16));
        let a1 = base.add(Angle::from_raw_q16(
            (segment as u16).wrapping_add(1).wrapping_mul(step as u16),
        ));
        let x0 = camera.position.x.saturating_add(a0.sin().mul_i32(radius));
        let z0 = camera.position.z.saturating_add(a0.cos().mul_i32(radius));
        let x1 = camera.position.x.saturating_add(a1.sin().mul_i32(radius));
        let z1 = camera.position.z.saturating_add(a1.cos().mul_i32(radius));
        let material = far_vista_texture_material(
            far_vista_panel_asset(vista, segment, segments),
            vista.tint_rgb,
        );
        if let Some((material, texture_width, texture_height)) = material {
            let options = options
                .with_depth_policy(DepthPolicy::Farthest)
                .with_cull_mode(CullMode::None)
                .with_material_layer(material);
            let _ = world.submit_textured_world_quad(
                triangles,
                camera,
                [
                    WorldVertex::new(x0, y1, z0),
                    WorldVertex::new(x1, y1, z1),
                    WorldVertex::new(x1, y0, z1),
                    WorldVertex::new(x0, y0, z0),
                ],
                [
                    (0, 0),
                    (texture_width.saturating_sub(1), 0),
                    (
                        texture_width.saturating_sub(1),
                        texture_height.saturating_sub(1),
                    ),
                    (0, texture_height.saturating_sub(1)),
                ],
                material,
                options,
            );
        }
    }
}

fn angle_from_signed_degrees(degrees: i16) -> Angle {
    Angle::from_degrees((degrees as i32).rem_euclid(360) as u32)
}

fn far_vista_panel_asset(vista: LevelFarVistaRecord, segment: u8, segments: u8) -> Option<AssetId> {
    if vista.flags & far_vista_flags::TEXTURED == 0 || vista.texture_assets.is_empty() {
        return None;
    }
    let panel_count = vista.texture_assets.len();
    let panel_index = if panel_count == 1 {
        0
    } else {
        ((segment as usize) * panel_count / (segments as usize).max(1)).min(panel_count - 1)
    };
    let asset = vista.texture_assets[panel_index];
    (asset.0 != u16::MAX).then_some(asset)
}

fn far_vista_texture_material(
    asset_id: Option<AssetId>,
    tint_rgb: [u8; 3],
) -> Option<(TextureMaterial, u8, u8)> {
    let asset = find_asset_of_kind(ASSETS, asset_id?, AssetKind::Texture)?;
    let slot = ensure_texture_uploaded_with_clut_mode(
        asset.id,
        asset.bytes,
        VramSlotClutMode::TransparentZero,
    )?;
    Some((
        TextureMaterial::opaque(slot.clut_word, slot.tpage_word, rgb_tuple(tint_rgb))
            .with_texture_window(slot.texture_window),
        vram_slot_texture_size_u8(slot.texture_width),
        vram_slot_texture_size_u8(slot.texture_height),
    ))
}

fn vram_slot_texture_size_u8(size: u16) -> u8 {
    size.min(u16::from(u8::MAX)) as u8
}

fn parse_runtime_room(record: &LevelRoomRecord) -> Option<RuntimeRoom<'static>> {
    let asset = find_asset_of_kind(ASSETS, record.world_asset, AssetKind::RoomWorld)?;
    RuntimeRoom::from_bytes(asset.bytes).ok()
}

fn parse_collision_room_for_index(
    index: RoomIndex,
    record: &LevelRoomRecord,
) -> Option<RuntimeCollisionRoom<'static>> {
    #[cfg(feature = "cd-stream-bench")]
    {
        let _ = record;
        parse_streamed_compact_collision_room(0, index).map(RuntimeCollisionRoom::Compact)
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        let _ = index;
        parse_runtime_room(record).map(RuntimeCollisionRoom::Runtime)
    }
}

#[derive(Copy, Clone)]
struct ParsedActiveRoomPayload {
    render_room: Option<RuntimeRoom<'static>>,
    collision_room: RuntimeCollisionRoom<'static>,
    width: u16,
    depth: u16,
    sector_size: i32,
    ambient_rgb: [u8; 3],
}

fn parse_active_room_payload(
    slot: usize,
    index: RoomIndex,
    record: &LevelRoomRecord,
) -> Option<ParsedActiveRoomPayload> {
    #[cfg(feature = "cd-stream-bench")]
    if let Some(room) = parse_streamed_compact_collision_room(slot, index) {
        return Some(ParsedActiveRoomPayload {
            render_room: None,
            collision_room: RuntimeCollisionRoom::Compact(room),
            width: room.width(),
            depth: room.depth(),
            sector_size: room.sector_size(),
            ambient_rgb: room.ambient_color(),
        });
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        let _ = (slot, index);
        let room = parse_runtime_room(record)?;
        Some(ParsedActiveRoomPayload {
            render_room: Some(room),
            collision_room: RuntimeCollisionRoom::Runtime(room),
            width: room.width(),
            depth: room.depth(),
            sector_size: room.sector_size(),
            ambient_rgb: room.render().ambient_color(),
        })
    }
    #[cfg(feature = "cd-stream-bench")]
    {
        let _ = record;
        None
    }
}

#[cfg(feature = "cd-stream-bench")]
fn parse_streamed_compact_collision_room(
    slot: usize,
    index: RoomIndex,
) -> Option<CompactCollisionRoom<'static>> {
    let _ = slot;
    unsafe {
        let resident_slot = ROOM_STREAM_SCHEDULER.resident_slot_for(index)?;
        let byte_count = ROOM_STREAM_SCHEDULER.resident_byte_count(resident_slot)?;
        let bytes = streamed_room_slot_bytes(resident_slot, byte_count)?;
        let view = streamed_room_chunk_view(bytes, index)?;
        if view.flags & STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT == 0 {
            return None;
        }
        let collision =
            bytes.get(view.collision_offset..view.collision_offset + view.collision_bytes)?;
        telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_HITS, 1);
        CompactCollisionRoom::from_bytes(collision).ok()
    }
}

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
struct StreamedRoomChunkView {
    total_bytes: usize,
    collision_offset: usize,
    collision_bytes: usize,
    cells_offset: usize,
    cell_count: usize,
    cell_vertices_offset: usize,
    cell_vertex_count: usize,
    vertices_offset: usize,
    vertex_count: usize,
    surfaces_offset: usize,
    surface_count: usize,
    flags: u32,
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_room_slot_bytes(slot: usize, byte_count: usize) -> Option<&'static [u8]> {
    if slot >= STREAMED_ROOM_SLOT_COUNT || byte_count > STREAMED_ROOM_SLOT_BYTES {
        return None;
    }
    unsafe {
        let ptr = core::ptr::addr_of!(STREAMED_ROOM_WORDS[slot])
            .cast::<u32>()
            .cast::<u8>();
        Some(core::slice::from_raw_parts(ptr, byte_count))
    }
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_room_chunk_view(
    bytes: &[u8],
    expected_room: RoomIndex,
) -> Option<StreamedRoomChunkView> {
    if bytes.len() < STREAMED_ROOM_CHUNK_HEADER_BYTES {
        return None;
    }
    if bytes.get(0..8)? != STREAMED_ROOM_CHUNK_MAGIC.as_slice() {
        return None;
    }
    if read_streamed_chunk_u32(bytes, streamed_room_chunk_header::VERSION)?
        != STREAMED_ROOM_CHUNK_VERSION
    {
        return None;
    }
    if read_streamed_chunk_u32(bytes, streamed_room_chunk_header::ROOM)?
        != u32::from(expected_room.raw())
    {
        return None;
    }
    let total_bytes =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::TOTAL_BYTES)? as usize;
    if total_bytes < STREAMED_ROOM_CHUNK_HEADER_BYTES || total_bytes > bytes.len() {
        return None;
    }
    let collision_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::COLLISION_OFFSET)? as usize;
    let collision_bytes =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::COLLISION_BYTES)? as usize;
    let cells_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::CELLS_OFFSET)? as usize;
    let cell_count =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::CELL_COUNT)? as usize;
    let cell_vertices_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::CELL_VERTICES_OFFSET)? as usize;
    let cell_vertex_count =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::CELL_VERTEX_COUNT)? as usize;
    let vertices_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::VERTICES_OFFSET)? as usize;
    let vertex_count =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::VERTEX_COUNT)? as usize;
    let surfaces_offset =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::SURFACES_OFFSET)? as usize;
    let surface_count =
        read_streamed_chunk_u32(bytes, streamed_room_chunk_header::SURFACE_COUNT)? as usize;
    let flags = read_streamed_chunk_u32(bytes, streamed_room_chunk_header::FLAGS)?;
    if !streamed_chunk_range_valid::<u8>(total_bytes, collision_offset, collision_bytes)
        || !streamed_chunk_range_valid::<LevelCachedRoomCellRecord>(
            total_bytes,
            cells_offset,
            cell_count,
        )
        || !streamed_chunk_range_valid::<u16>(total_bytes, cell_vertices_offset, cell_vertex_count)
        || !streamed_chunk_range_valid::<LevelCachedRoomVertexRecord>(
            total_bytes,
            vertices_offset,
            vertex_count,
        )
        || !streamed_chunk_range_valid::<LevelCachedRoomSurfaceRecord>(
            total_bytes,
            surfaces_offset,
            surface_count,
        )
    {
        return None;
    }
    Some(StreamedRoomChunkView {
        total_bytes,
        collision_offset,
        collision_bytes,
        cells_offset,
        cell_count,
        cell_vertices_offset,
        cell_vertex_count,
        vertices_offset,
        vertex_count,
        surfaces_offset,
        surface_count,
        flags,
    })
}

#[cfg(feature = "cd-stream-bench")]
fn read_streamed_chunk_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_chunk_range_valid<T>(total_bytes: usize, offset: usize, count: usize) -> bool {
    if count == 0 {
        return offset <= total_bytes;
    }
    if offset % core::mem::align_of::<T>() != 0 {
        return false;
    }
    let Some(byte_count) = count.checked_mul(core::mem::size_of::<T>()) else {
        return false;
    };
    offset
        .checked_add(byte_count)
        .is_some_and(|end| end <= total_bytes)
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_room_is_resident(index: RoomIndex) -> bool {
    unsafe { ROOM_STREAM_SCHEDULER.is_resident(index) }
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_room_is_loading(index: RoomIndex) -> bool {
    unsafe { ROOM_STREAM_SCHEDULER.is_loading(index) }
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_room_stream_active() -> bool {
    unsafe { ROOM_STREAM_SCHEDULER.job.is_active() }
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_slot_reserved(slot: usize, reserved_slots: &[usize], reserved_count: usize) -> bool {
    let mut i = 0usize;
    while i < reserved_count.min(reserved_slots.len()) {
        if reserved_slots[i] == slot {
            return true;
        }
        i += 1;
    }
    false
}

/// Breadth-first room-graph ring around `start`.
///
/// Walks the portal connectivity graph (portals are edges, rooms are nodes) in
/// distance order and writes the rooms reachable within `max_depth` portal hops
/// into `out`, stopping once `out_cap` rooms are written. Because expansion is
/// distance-ordered, capping keeps the NEAREST rooms. Returns the number of
/// rooms written.
///
/// Neighbours of room `r` are the `destination_room`s of the portals in
/// `ROOM_PORTALS[r.portal_first .. r.portal_first + r.portal_count]`. Invalid
/// indices and indices outside `ROOMS` are skipped.
fn room_graph_ring(
    start: RoomIndex,
    max_depth: u16,
    out: &mut [RoomIndex],
    out_cap: usize,
) -> usize {
    let mut count = 0usize;
    if start == INVALID_ROOM_INDEX
        || start.to_usize() >= ROOMS.len()
        || start.to_usize() >= MAX_STREAMED_ROOM_INDEX_COUNT
        || out_cap == 0
    {
        return count;
    }

    let mut visited = [false; MAX_STREAMED_ROOM_INDEX_COUNT];
    let mut queue = [(INVALID_ROOM_INDEX, 0u16); MAX_STREAMED_ROOM_INDEX_COUNT];
    let mut head = 0usize;
    let mut tail = 0usize;

    visited[start.to_usize()] = true;
    queue[tail] = (start, 0u16);
    tail += 1;

    while head < tail {
        let (room, depth) = queue[head];
        head += 1;

        if count < out_cap {
            out[count] = room;
            count += 1;
        } else {
            break;
        }

        if depth >= max_depth {
            continue;
        }

        let Some(record) = ROOMS.get(room.to_usize()) else {
            continue;
        };
        let portal_first = record.portal_first as usize;
        let portal_end = portal_first.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_first;
        while portal_index < portal_end.min(ROOM_PORTALS.len()) {
            let portal = ROOM_PORTALS[portal_index];
            portal_index += 1;
            if portal.source_room != room {
                continue;
            }
            let neighbour = portal.destination_room;
            if neighbour == INVALID_ROOM_INDEX {
                continue;
            }
            let neighbour_idx = neighbour.to_usize();
            if neighbour_idx >= ROOMS.len() || neighbour_idx >= MAX_STREAMED_ROOM_INDEX_COUNT {
                continue;
            }
            if visited[neighbour_idx] {
                continue;
            }
            if tail >= MAX_STREAMED_ROOM_INDEX_COUNT {
                continue;
            }
            visited[neighbour_idx] = true;
            queue[tail] = (neighbour, depth + 1);
            tail += 1;
        }
    }

    count
}

#[cfg(feature = "cd-stream-bench")]
fn room_requested(
    room: RoomIndex,
    requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
    requested_count: usize,
) -> bool {
    let mut i = 0usize;
    while i < requested_count {
        if requested_rooms[i] == room {
            return true;
        }
        i += 1;
    }
    false
}

// Retained after the BFS-ring residency rewrite (the desired-set is now copied
// from the cached stream ring); kept for other build paths / future reuse.
const fn room_material_fallback() -> WorldRenderMaterial {
    WorldRenderMaterial::both(TextureMaterial::opaque(0, TPAGE_WORD, (0x80, 0x80, 0x80)))
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
impl Playtest {
    fn clear_visible_cell_caches(&mut self) {
        self.visible_cell_caches = [const { ActiveVisibleCellCache::EMPTY }; MAX_ACTIVE_ROOMS];
        self.visible_cell_cache_cursor = 0;
    }

    fn cached_precomputed_visible_cells(
        &mut self,
        active_slot: usize,
        room_index: RoomIndex,
        room_width: u16,
        room_depth: u16,
        room_sector_size: i32,
        anchor: RoomPoint,
        room_offset_x: i32,
        room_offset_z: i32,
        global_anchor: RoomPoint,
        camera: WorldCamera,
        camera_independent: bool,
    ) -> Option<(&[GridVisibleCell], u16)> {
        let sector_size = room_sector_size.max(1);
        let anchor_x = grid_cell_for_room(anchor.x, sector_size).clamp(0, room_width as i32 - 1);
        let anchor_z = grid_cell_for_room(anchor.z, sector_size).clamp(0, room_depth as i32 - 1);
        let (view_sin_key, view_cos_key) = visible_cell_view_keys(camera, camera_independent);
        let cache = *self.visible_cell_caches.get(active_slot)?;
        if cache.ready
            && cache.room == room_index
            && cache.anchor_x == anchor_x
            && cache.anchor_z == anchor_z
            && cache.view_sin_key == view_sin_key
            && cache.view_cos_key == view_cos_key
            && cache.camera_independent == camera_independent
        {
            let first = cache.first as usize;
            let count = cache.count as usize;
            let end = first.checked_add(count)?;
            return self
                .visible_cell_cache_cells
                .get(first..end)
                .map(|cells| (cells, cache.rejected_global));
        }

        let mut first = self.visible_cell_cache_cursor;
        if MAX_ACTIVE_VISIBLE_CELLS.saturating_sub(first) < MAX_PRECOMPUTED_VISIBLE_CELLS {
            self.clear_visible_cell_caches();
            first = 0;
        }
        let (mut count, mut rejected_global) = {
            let cells = self.visible_cell_cache_cells.get_mut(first..)?;
            let depths = unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_DEPTHS[..] };
            fill_precomputed_visible_cells(
                room_index,
                anchor_x,
                anchor_z,
                room_offset_x,
                room_offset_z,
                sector_size,
                global_anchor,
                camera,
                camera_independent,
                cells,
                depths,
            )
        }?;

        if first.saturating_add(count) > MAX_ACTIVE_VISIBLE_CELLS || count > u16::MAX as usize {
            self.clear_visible_cell_caches();
            first = 0;
            (count, rejected_global) = {
                let cells = self.visible_cell_cache_cells.get_mut(first..)?;
                let depths = unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_DEPTHS[..] };
                fill_precomputed_visible_cells(
                    room_index,
                    anchor_x,
                    anchor_z,
                    room_offset_x,
                    room_offset_z,
                    sector_size,
                    global_anchor,
                    camera,
                    camera_independent,
                    cells,
                    depths,
                )
            }?;
            if count > MAX_ACTIVE_VISIBLE_CELLS || count > u16::MAX as usize {
                return None;
            }
        }

        self.visible_cell_caches[active_slot] = ActiveVisibleCellCache {
            room: room_index,
            anchor_x,
            anchor_z,
            view_sin_key,
            view_cos_key,
            camera_independent,
            rejected_global,
            first: first as u16,
            count: count as u16,
            ready: true,
        };
        self.visible_cell_cache_cursor = first.saturating_add(count);
        self.visible_cell_cache_cells
            .get(first..self.visible_cell_cache_cursor)
            .map(|cells| (cells, rejected_global))
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn fill_precomputed_visible_cells(
    room_index: RoomIndex,
    anchor_x: i32,
    anchor_z: i32,
    room_offset_x: i32,
    room_offset_z: i32,
    sector_size: i32,
    global_anchor: RoomPoint,
    camera: WorldCamera,
    camera_independent: bool,
    out: &mut [GridVisibleCell],
    depths: &mut [i32],
) -> Option<(usize, u16)> {
    let room_visibility = ROOM_VISIBILITY
        .iter()
        .find(|visibility| visibility.room == room_index)?;
    let room_record = ROOMS.get(room_index.to_usize())?;
    let first = room_visibility.cell_first.to_usize();
    let count = room_visibility.cell_count as usize;
    if count > out.len() || count > depths.len() || count > MAX_PRECOMPUTED_VISIBLE_CELLS {
        return None;
    }
    let room_cells = VISIBILITY_CELLS.get(first..first.checked_add(count)?)?;
    let anchor_index = visibility_cell_index_for_anchor(room_cells, anchor_x, anchor_z)
        .or_else(|| nearest_runtime_visibility_cell(room_cells, anchor_x, anchor_z))?;
    let pvs_index = (room_visibility.pvs_first as usize).checked_add(anchor_index)?;
    if anchor_index >= room_visibility.pvs_count as usize {
        return None;
    }
    let pvs = *VISIBILITY_PVS.get(pvs_index)?;
    let byte_first = pvs.byte_first as usize;
    let byte_end = byte_first.checked_add(pvs.byte_count as usize)?;
    let pvs_bits = VISIBILITY_PVS_BITS.get(byte_first..byte_end)?;
    let filter = VisibleCellFilter {
        anchor_x,
        anchor_z,
        sector_size,
        room_offset_x,
        room_offset_z,
        global_anchor,
        camera,
        camera_independent,
        far_z: room_draw_distance(room_record),
        global_radius_sectors: room_chunk_activation_radius_sectors(room_record),
    };
    let mut written = 0usize;
    let mut rejected_global = 0u16;
    let mut cell_index = 0usize;
    while cell_index < room_cells.len() {
        if visibility_pvs_bit(pvs_bits, cell_index) {
            write_visible_cell_candidate(
                room_cells[cell_index],
                filter,
                out,
                depths,
                &mut written,
                &mut rejected_global,
            );
        }
        cell_index += 1;
    }
    sort_visible_cells_for_camera(&mut out[..written], &mut depths[..written]);
    Some((written, rejected_global))
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visible_cell_view_keys(camera: WorldCamera, camera_independent: bool) -> (i16, i16) {
    if camera_independent {
        let _ = camera;
        return (0, 0);
    }
    #[cfg(any(feature = "vis-anchor-cache", feature = "vis-anchor-pvs-candidates"))]
    {
        let _ = camera;
        let _ = camera_independent;
        (0, 0)
    }
    #[cfg(all(
        not(feature = "vis-anchor-cache"),
        not(feature = "vis-anchor-pvs-candidates"),
        feature = "vis-coarse-yaw"
    ))]
    {
        (
            (camera.sin_yaw.raw() / 2048) as i16,
            (camera.cos_yaw.raw() / 2048) as i16,
        )
    }
    #[cfg(all(
        not(feature = "vis-anchor-cache"),
        not(feature = "vis-anchor-pvs-candidates"),
        not(feature = "vis-coarse-yaw")
    ))]
    {
        (
            (camera.sin_yaw.raw() / 256) as i16,
            (camera.cos_yaw.raw() / 256) as i16,
        )
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn sort_visible_cells_for_camera(cells: &mut [GridVisibleCell], depths: &mut [i32]) {
    if cells.len() > depths.len() {
        return;
    }
    let mut gap = cells.len() / 2;
    while gap > 0 {
        let mut i = gap;
        while i < cells.len() {
            let cell = cells[i];
            let depth = depths[i];
            let mut j = i;
            while j >= gap && depths[j - gap] < depth {
                cells[j] = cells[j - gap];
                depths[j] = depths[j - gap];
                j -= gap;
            }
            cells[j] = cell;
            depths[j] = depth;
            i += 1;
        }
        gap /= 2;
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visible_cell_camera_depth_if_sphere_visible(
    cell: psx_level::LevelVisibilityCellRecord,
    camera: WorldCamera,
    sector_size: i32,
    far_z: i32,
) -> Option<i32> {
    let sector_size = sector_size.max(1);
    let half = sector_size / 2;
    let center = WorldVertex::new(
        (cell.x as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
        cell.min_y.saturating_add(cell.max_y) / 2,
        (cell.z as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
    );
    let half_height = ((cell.max_y - cell.min_y).abs() / 2).max(half);
    let radius = sector_size.saturating_add(half_height);
    let view = camera.view_vertex(center);
    let near = camera.projection.near_z.max(1);
    let far = far_z.max(near);
    if view.z < near.saturating_sub(radius) || view.z > far.saturating_add(radius) {
        return None;
    }

    let z = view.z.max(near);
    let focal = camera.projection.focal_length.max(1);
    let half_w = (camera.projection.screen_x as i32)
        .saturating_add(ROOM_VISIBLE_CELL_SCREEN_MARGIN)
        .max(1);
    let half_h = (camera.projection.screen_y as i32)
        .saturating_add(ROOM_VISIBLE_CELL_SCREEN_MARGIN)
        .max(1);
    let projected_x = view.x.abs().saturating_sub(radius).saturating_mul(focal);
    let projected_y = view.y.abs().saturating_sub(radius).saturating_mul(focal);
    if projected_x > half_w.saturating_mul(z) || projected_y > half_h.saturating_mul(z) {
        return None;
    }
    Some(view.z)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visible_cell_camera_depth(
    cell: psx_level::LevelVisibilityCellRecord,
    camera: WorldCamera,
    sector_size: i32,
) -> i32 {
    let sector_size = sector_size.max(1);
    let half = sector_size / 2;
    let center = WorldVertex::new(
        (cell.x as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
        cell.min_y.saturating_add(cell.max_y) / 2,
        (cell.z as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
    );
    camera.view_vertex(center).z
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
#[derive(Copy, Clone)]
struct VisibleCellFilter {
    anchor_x: i32,
    anchor_z: i32,
    sector_size: i32,
    room_offset_x: i32,
    room_offset_z: i32,
    global_anchor: RoomPoint,
    camera: WorldCamera,
    camera_independent: bool,
    far_z: i32,
    global_radius_sectors: i32,
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
#[derive(Copy, Clone, PartialEq, Eq)]
enum VisibleCellReject {
    GlobalRange,
    Camera,
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn write_visible_cell_candidate(
    cell: psx_level::LevelVisibilityCellRecord,
    filter: VisibleCellFilter,
    out: &mut [GridVisibleCell],
    depths: &mut [i32],
    written: &mut usize,
    rejected_global: &mut u16,
) {
    match visible_cell_reject_reason(cell, filter) {
        Some(VisibleCellReject::GlobalRange) => {
            *rejected_global = rejected_global.saturating_add(1);
            return;
        }
        Some(VisibleCellReject::Camera) => return,
        None => {}
    }
    if *written >= out.len() {
        return;
    }
    let visible_cell = GridVisibleCell::with_cache_cell_index(
        cell.x,
        cell.z,
        cell.min_y,
        cell.max_y,
        cell.cache_cell_index,
    );
    if filter.camera_independent || cfg!(feature = "vis-anchor-pvs-candidates") {
        out[*written] = visible_cell;
        depths[*written] = 0;
        *written += 1;
        return;
    }
    let depth = if cfg!(feature = "vis-broad-pvs") {
        visible_cell_camera_depth(cell, filter.camera, filter.sector_size)
    } else {
        let Some(depth) = visible_cell_camera_depth_if_sphere_visible(
            cell,
            filter.camera,
            filter.sector_size,
            filter.far_z,
        ) else {
            return;
        };
        out[*written] = visible_cell.with_camera_depth(GridVisibleCell::CAMERA_DEPTH_PRECULLED);
        depths[*written] = depth;
        *written += 1;
        return;
    };
    out[*written] = visible_cell;
    depths[*written] = depth;
    *written += 1;
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visible_cell_reject_reason(
    cell: psx_level::LevelVisibilityCellRecord,
    filter: VisibleCellFilter,
) -> Option<VisibleCellReject> {
    if visibility_cell_safety_ring(cell, filter.anchor_x, filter.anchor_z) {
        return None;
    }
    if !visibility_cell_in_global_range(
        cell.x,
        cell.z,
        filter.sector_size,
        filter.room_offset_x,
        filter.room_offset_z,
        filter.global_anchor,
        filter.global_radius_sectors,
    ) {
        return Some(VisibleCellReject::GlobalRange);
    }
    if cfg!(feature = "vis-broad-pvs") {
        return None;
    }
    if filter.camera_independent || cfg!(feature = "vis-anchor-pvs-candidates") {
        return None;
    }
    if !visibility_cell_in_view_wedge(cell, filter) {
        return Some(VisibleCellReject::Camera);
    }
    if !visibility_cell_aabb_intersects_camera(
        cell,
        filter.sector_size,
        filter.camera,
        filter.far_z,
    ) {
        return Some(VisibleCellReject::Camera);
    }
    None
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_safety_ring(
    cell: psx_level::LevelVisibilityCellRecord,
    anchor_x: i32,
    anchor_z: i32,
) -> bool {
    visibility_cell_anchor_distance(cell, anchor_x, anchor_z) <= ROOM_VISIBLE_CELL_SAFETY_RING
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_anchor_distance(
    cell: psx_level::LevelVisibilityCellRecord,
    anchor_x: i32,
    anchor_z: i32,
) -> i32 {
    let dx = (cell.x as i32).saturating_sub(anchor_x).abs();
    let dz = (cell.z as i32).saturating_sub(anchor_z).abs();
    dx.max(dz)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_in_view_wedge(
    cell: psx_level::LevelVisibilityCellRecord,
    filter: VisibleCellFilter,
) -> bool {
    let anchor_distance = visibility_cell_anchor_distance(cell, filter.anchor_x, filter.anchor_z);
    if anchor_distance <= ROOM_VISIBLE_CELL_NEAR_RING {
        return true;
    }
    if cell.blocker_mask != 0 || cell.portal_mask != 0x0f {
        return true;
    }

    let sector_size = filter.sector_size.max(1);
    let half = sector_size / 2;
    let center_x = (cell.x as i32)
        .saturating_mul(sector_size)
        .saturating_add(half);
    let center_z = (cell.z as i32)
        .saturating_mul(sector_size)
        .saturating_add(half);
    let anchor_x = filter
        .anchor_x
        .saturating_mul(sector_size)
        .saturating_add(half);
    let anchor_z = filter
        .anchor_z
        .saturating_mul(sector_size)
        .saturating_add(half);
    let dx = center_x.saturating_sub(anchor_x);
    let dz = center_z.saturating_sub(anchor_z);
    let sin_yaw = filter.camera.sin_yaw.raw();
    let cos_yaw = filter.camera.cos_yaw.raw();
    let forward_x = -sin_yaw;
    let forward_z = -cos_yaw;
    let depth = mul_q12_i32(dx, forward_x).saturating_add(mul_q12_i32(dz, forward_z));
    if depth < 0 {
        return anchor_distance <= ROOM_VISIBLE_CELL_REAR_RING;
    }
    let lateral = mul_q12_i32(dx, cos_yaw)
        .saturating_sub(mul_q12_i32(dz, sin_yaw))
        .unsigned_abs();
    let lateral_limit = depth
        .saturating_mul(ROOM_VISIBLE_CELL_WEDGE_NUM)
        .checked_div(ROOM_VISIBLE_CELL_WEDGE_DEN.max(1))
        .unwrap_or(i32::MAX)
        .saturating_add(sector_size.saturating_mul(ROOM_VISIBLE_CELL_WEDGE_MARGIN_SECTORS))
        .max(0)
        .unsigned_abs();
    lateral <= lateral_limit
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_aabb_intersects_camera(
    cell: psx_level::LevelVisibilityCellRecord,
    sector_size: i32,
    camera: WorldCamera,
    far_z: i32,
) -> bool {
    let sector_size = sector_size.max(1);
    let margin = ROOM_VISIBLE_CELL_CAMERA_MARGIN.max(sector_size / 4);
    let x0 = (cell.x as i32)
        .saturating_mul(sector_size)
        .saturating_sub(margin);
    let x1 = (cell.x as i32)
        .saturating_add(1)
        .saturating_mul(sector_size)
        .saturating_add(margin);
    let z0 = (cell.z as i32)
        .saturating_mul(sector_size)
        .saturating_sub(margin);
    let z1 = (cell.z as i32)
        .saturating_add(1)
        .saturating_mul(sector_size)
        .saturating_add(margin);
    let y0 = cell.min_y.saturating_sub(margin);
    let y1 = cell.max_y.saturating_add(margin);
    aabb_intersects_camera_frustum(x0, x1, y0, y1, z0, z1, camera, far_z)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn aabb_intersects_camera_frustum(
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
    z0: i32,
    z1: i32,
    camera: WorldCamera,
    far_z: i32,
) -> bool {
    let near = camera.projection.near_z.max(1);
    let far = far_z.max(near);
    let focal = camera.projection.focal_length.max(1);
    let half_w = (camera.projection.screen_x as i32)
        .saturating_add(ROOM_VISIBLE_CELL_CAMERA_MARGIN)
        .max(1);
    let half_h = (camera.projection.screen_y as i32)
        .saturating_add(ROOM_VISIBLE_CELL_CAMERA_MARGIN)
        .max(1);
    let mut max_depth = i32::MIN;
    let mut min_depth = i32::MAX;
    let mut all_right = true;
    let mut all_left = true;
    let mut all_above = true;
    let mut all_below = true;
    for x in [x0, x1] {
        for y in [y0, y1] {
            for z in [z0, z1] {
                let view = camera.view_vertex(WorldVertex::new(x, y, z));
                max_depth = max_depth.max(view.z);
                min_depth = min_depth.min(view.z);
                if view.z < near {
                    all_right = false;
                    all_left = false;
                    all_above = false;
                    all_below = false;
                    continue;
                }
                let depth_limit_x = half_w.saturating_mul(view.z);
                let depth_limit_y = half_h.saturating_mul(view.z);
                let projected_x = view.x.saturating_mul(focal);
                let projected_y = view.y.saturating_mul(focal);
                if projected_x <= depth_limit_x {
                    all_right = false;
                }
                if -projected_x <= depth_limit_x {
                    all_left = false;
                }
                if projected_y <= depth_limit_y {
                    all_above = false;
                }
                if -projected_y <= depth_limit_y {
                    all_below = false;
                }
            }
        }
    }
    if max_depth < near || min_depth > far {
        return false;
    }
    !(all_right || all_left || all_above || all_below)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_in_global_range(
    x: u16,
    z: u16,
    sector_size: i32,
    room_offset_x: i32,
    room_offset_z: i32,
    global_anchor: RoomPoint,
    radius_sectors: i32,
) -> bool {
    let radius = radius_sectors.max(1).saturating_mul(sector_size);
    let x0 = room_offset_x.saturating_add((x as i32).saturating_mul(sector_size));
    let z0 = room_offset_z.saturating_add((z as i32).saturating_mul(sector_size));
    let x1 = x0.saturating_add(sector_size);
    let z1 = z0.saturating_add(sector_size);
    rect_distance_sq(global_anchor.x, global_anchor.z, x0, x1, z0, z1)
        <= square_i32_to_u32_saturating(radius)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_pvs_bit(bits: &[u8], index: usize) -> bool {
    let byte = index / 8;
    let bit = index % 8;
    bits.get(byte)
        .map(|value| value & (1 << bit) != 0)
        .unwrap_or(false)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_index_for_anchor(
    cells: &[psx_level::LevelVisibilityCellRecord],
    x: i32,
    z: i32,
) -> Option<usize> {
    if x < 0 || z < 0 || x > u16::MAX as i32 || z > u16::MAX as i32 {
        return None;
    }
    visibility_cell_index_by_coord(cells, x as u16, z as u16)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_index_by_coord(
    cells: &[psx_level::LevelVisibilityCellRecord],
    x: u16,
    z: u16,
) -> Option<usize> {
    let key = visibility_cell_key(x, z);
    let mut low = 0usize;
    let mut high = cells.len();
    while low < high {
        let mid = (low + high) / 2;
        let cell = cells[mid];
        if visibility_cell_key(cell.x, cell.z) < key {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let cell = cells.get(low)?;
    (visibility_cell_key(cell.x, cell.z) == key).then_some(low)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const fn visibility_cell_key(x: u16, z: u16) -> u32 {
    ((x as u32) << 16) | z as u32
}

const INVALID_ACTIVE_ROOM_SLOT: u8 = u8::MAX;

fn active_room_draw_order(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
    mode: CachedRoomDrawOrderMode,
) -> [u8; MAX_ACTIVE_ROOMS] {
    match mode {
        CachedRoomDrawOrderMode::Distance => {
            active_room_draw_order_by_distance(active_rooms, camera, visibility, current_room)
        }
        CachedRoomDrawOrderMode::Portal => {
            active_room_draw_order_by_portal(active_rooms, visibility, current_room)
        }
        CachedRoomDrawOrderMode::Slot => {
            active_room_draw_order_by_slot(active_rooms, visibility, current_room)
        }
    }
}

fn active_room_draw_order_by_distance(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut depths = [i32::MIN; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if !portal_visibility_result_draws_room(visibility, current_room, active.index) {
                slot += 1;
                continue;
            }
            let depth = active_room_sort_depth(active, camera);
            let mut insert = count;
            while insert > 0 && depth > depths[insert - 1] {
                depths[insert] = depths[insert - 1];
                order[insert] = order[insert - 1];
                insert -= 1;
            }
            depths[insert] = depth;
            order[insert] = slot as u8;
            count += 1;
        }
        slot += 1;
    }
    order
}

fn active_room_draw_order_by_portal(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut visible_index = 0usize;
    while visible_index < visibility.room_count.min(MAX_ACTIVE_ROOMS) && count < MAX_ACTIVE_ROOMS {
        let room = visibility.rooms[visible_index].room;
        if let Some(slot) = active_room_slot_for_room(active_rooms, room) {
            order[count] = slot;
            count += 1;
        }
        visible_index += 1;
    }
    if count == 0 {
        if let Some(slot) = active_room_slot_for_room(active_rooms, current_room) {
            order[count] = slot;
            count += 1;
        }
    }
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS && count < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if portal_visibility_result_draws_room(visibility, current_room, active.index)
                && !active_draw_order_contains(&order, count, slot as u8)
            {
                order[count] = slot as u8;
                count += 1;
            }
        }
        slot += 1;
    }
    order
}

fn active_room_draw_order_by_slot(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if portal_visibility_result_draws_room(visibility, current_room, active.index) {
                order[count] = slot as u8;
                count += 1;
            }
        }
        slot += 1;
    }
    order
}

fn active_room_slot_for_room(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    room: RoomIndex,
) -> Option<u8> {
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if active_rooms[slot].is_some_and(|active| active.index == room) {
            return Some(slot as u8);
        }
        slot += 1;
    }
    None
}

fn active_draw_order_contains(order: &[u8; MAX_ACTIVE_ROOMS], count: usize, slot: u8) -> bool {
    let mut i = 0usize;
    while i < count.min(MAX_ACTIVE_ROOMS) {
        if order[i] == slot {
            return true;
        }
        i += 1;
    }
    false
}

fn portal_visibility_result_draws_room(
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
    index: RoomIndex,
) -> bool {
    if index == current_room {
        return true;
    }
    let mut i = 0usize;
    while i < visibility.room_count.min(MAX_ACTIVE_ROOMS) {
        if visibility.rooms[i].room == index {
            // Visible (and resident for prefetch) but only drawn when within
            // the draw distance -- a beyond-far room renders nothing.
            return visibility.rooms[i].within_far;
        }
        i += 1;
    }
    false
}

fn active_room_sort_depth(active: ActiveRuntimeRoom, camera: WorldCamera) -> i32 {
    let sector_size = active.sector_size.max(1);
    let center_x = active
        .offset_x
        .saturating_add((active.width as i32).saturating_mul(sector_size) / 2);
    let center_z = active
        .offset_z
        .saturating_add((active.depth as i32).saturating_mul(sector_size) / 2);
    camera
        .view_vertex(WorldVertex::new(center_x, 0, center_z))
        .z
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn nearest_runtime_visibility_cell(
    cells: &[psx_level::LevelVisibilityCellRecord],
    x: i32,
    z: i32,
) -> Option<usize> {
    let mut best_index = None;
    let mut best_score = u32::MAX;
    for (index, cell) in cells.iter().enumerate() {
        let dx = (cell.x as i32).saturating_sub(x).unsigned_abs();
        let dz = (cell.z as i32).saturating_sub(z).unsigned_abs();
        let score = dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz));
        if best_index.is_none() || score < best_score {
            best_index = Some(index);
            best_score = score;
        }
    }
    best_index
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn grid_cell_for_room(value: i32, sector_size: i32) -> i32 {
    if value >= 0 {
        value / sector_size
    } else {
        (value - sector_size + 1) / sector_size
    }
}

fn build_active_room(
    slot: usize,
    index: RoomIndex,
    record: &LevelRoomRecord,
    current_record: &LevelRoomRecord,
) -> Option<ActiveRuntimeRoom> {
    if let Some(residency) = ROOM_RESIDENCY.iter().find(|r| r.room == index) {
        let _ = unsafe { RESIDENCY.ensure_room_resident(residency) };
    }
    let payload = parse_active_room_payload(slot, index, record)?;
    let (materials, material_count) = build_runtime_room_material_table(record);
    let surface_cache = active_room_surface_cache_for(index);
    Some(ActiveRuntimeRoom {
        index,
        stream_slot: active_room_stream_slot(index),
        render_room: payload.render_room,
        collision_room: payload.collision_room,
        width: payload.width,
        depth: payload.depth,
        sector_size: payload.sector_size,
        ambient_rgb: payload.ambient_rgb,
        materials,
        material_count,
        offset_x: room_origin_x(record).saturating_sub(room_origin_x(current_record)),
        offset_z: room_origin_z(record).saturating_sub(room_origin_z(current_record)),
        surface_cache,
    })
}

fn reuse_or_build_active_room(
    slot: usize,
    index: RoomIndex,
    record: &LevelRoomRecord,
    current_record: &LevelRoomRecord,
    previous_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
) -> Option<ActiveRuntimeRoom> {
    let stream_slot = active_room_stream_slot(index);
    for previous in previous_rooms.iter().flatten().copied() {
        if previous.index != index || previous.stream_slot != stream_slot {
            continue;
        }
        return Some(previous.with_current_room_offsets(record, current_record));
    }
    build_active_room(slot, index, record, current_record)
}

fn active_room_stream_slot(index: RoomIndex) -> u16 {
    #[cfg(feature = "cd-stream-bench")]
    unsafe {
        ROOM_STREAM_SCHEDULER
            .resident_slot_for(index)
            .and_then(|slot| u16::try_from(slot).ok())
            .unwrap_or(STREAMED_ROOM_SLOT_NONE)
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        let _ = index;
        u16::MAX
    }
}

fn build_runtime_room_material_table(
    record: &LevelRoomRecord,
) -> ([WorldRenderMaterial; MAX_ROOM_MATERIALS], usize) {
    let mut resolved_materials = [const { None }; MAX_ROOM_MATERIALS];
    let material_count = build_room_materials(record, &mut resolved_materials);
    let mut materials = [room_material_fallback(); MAX_ROOM_MATERIALS];
    for i in 0..material_count {
        if let Some(material) = resolved_materials[i] {
            materials[i] = material;
        }
    }
    (materials, material_count)
}

fn active_room_surface_cache_for(index: RoomIndex) -> ActiveRoomSurfaceCache {
    #[cfg(feature = "cd-stream-bench")]
    if let Some(cache) = streamed_active_room_surface_cache_for(index) {
        return cache;
    }

    let Some(cache) = ROOM_SURFACE_CACHES.iter().find(|cache| cache.room == index) else {
        return ActiveRoomSurfaceCache::EMPTY;
    };
    let cell_first = cache.cell_first as usize;
    let cell_count = cache.cell_count as usize;
    let cell_vertex_first = cache.cell_vertex_first as usize;
    let cell_vertex_count = cache.cell_vertex_count as usize;
    let vertex_first = cache.vertex_first as usize;
    let vertex_count = cache.vertex_count as usize;
    let surface_first = cache.surface_first as usize;
    let surface_count = cache.surface_count as usize;
    if vertex_count > MAX_CACHED_ROOM_VERTICES
        || cell_first.saturating_add(cell_count) > ROOM_CACHE_CELLS.len()
        || cell_vertex_first.saturating_add(cell_vertex_count) > ROOM_CACHE_CELL_VERTICES.len()
        || vertex_first.saturating_add(vertex_count) > ROOM_CACHE_VERTICES.len()
        || surface_first.saturating_add(surface_count) > ROOM_CACHE_SURFACES.len()
    {
        return ActiveRoomSurfaceCache {
            status: ActiveRoomCacheStatus::Overflow,
            ..ActiveRoomSurfaceCache::EMPTY
        };
    }
    if cell_count == 0 || vertex_count == 0 || surface_count == 0 {
        return ActiveRoomSurfaceCache {
            status: ActiveRoomCacheStatus::Empty,
            ..ActiveRoomSurfaceCache::EMPTY
        };
    }
    ActiveRoomSurfaceCache {
        cell_first,
        cell_count,
        cell_vertex_first,
        cell_vertex_count,
        vertex_first,
        vertex_count,
        surface_first,
        surface_count,
        status: ActiveRoomCacheStatus::Ready,
        ready: true,
    }
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_active_room_surface_cache_for(index: RoomIndex) -> Option<ActiveRoomSurfaceCache> {
    unsafe {
        let resident_slot = ROOM_STREAM_SCHEDULER.resident_slot_for(index)?;
        let byte_count = ROOM_STREAM_SCHEDULER.resident_byte_count(resident_slot)?;
        let bytes = streamed_room_slot_bytes(resident_slot, byte_count)?;
        let view = streamed_room_chunk_view(bytes, index)?;
        if view.vertex_count > MAX_CACHED_ROOM_VERTICES {
            return Some(ActiveRoomSurfaceCache {
                status: ActiveRoomCacheStatus::Overflow,
                ..ActiveRoomSurfaceCache::EMPTY
            });
        }
        if view.cell_count == 0 || view.vertex_count == 0 || view.surface_count == 0 {
            return Some(ActiveRoomSurfaceCache {
                status: ActiveRoomCacheStatus::Empty,
                ..ActiveRoomSurfaceCache::EMPTY
            });
        }
        Some(ActiveRoomSurfaceCache {
            cell_first: view.cells_offset,
            cell_count: view.cell_count,
            cell_vertex_first: view.cell_vertices_offset,
            cell_vertex_count: view.cell_vertex_count,
            vertex_first: view.vertices_offset,
            vertex_count: view.vertex_count,
            surface_first: view.surfaces_offset,
            surface_count: view.surface_count,
            status: ActiveRoomCacheStatus::Ready,
            ready: true,
        })
    }
}

fn room_surface_cache_slices(
    index: RoomIndex,
    cache: ActiveRoomSurfaceCache,
) -> Option<(
    &'static [CachedRoomCell],
    &'static [u16],
    &'static [WorldVertex],
    &'static [CachedRoomSurface],
)> {
    #[cfg(feature = "cd-stream-bench")]
    if let Some(slices) = streamed_room_surface_cache_slices(index, cache) {
        return Some(slices);
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    let _ = index;

    generated_room_surface_cache_slices(cache)
}

fn generated_room_surface_cache_slices(
    cache: ActiveRoomSurfaceCache,
) -> Option<(
    &'static [CachedRoomCell],
    &'static [u16],
    &'static [WorldVertex],
    &'static [CachedRoomSurface],
)> {
    if !cache.ready || cache.vertex_count > MAX_CACHED_ROOM_VERTICES {
        return None;
    }
    let cell_end = cache.cell_first.checked_add(cache.cell_count)?;
    let cell_vertex_end = cache
        .cell_vertex_first
        .checked_add(cache.cell_vertex_count)?;
    let vertex_end = cache.vertex_first.checked_add(cache.vertex_count)?;
    let surface_end = cache.surface_first.checked_add(cache.surface_count)?;
    let cells = ROOM_CACHE_CELLS.get(cache.cell_first..cell_end)?;
    let cell_vertices = ROOM_CACHE_CELL_VERTICES.get(cache.cell_vertex_first..cell_vertex_end)?;
    let vertices = ROOM_CACHE_VERTICES.get(cache.vertex_first..vertex_end)?;
    let surfaces = ROOM_CACHE_SURFACES.get(cache.surface_first..surface_end)?;
    Some((
        cached_room_cells_from_level_records(cells),
        cell_vertices,
        cached_room_vertices_from_level_records(vertices),
        cached_room_surfaces_from_level_records(surfaces),
    ))
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_room_surface_cache_slices(
    index: RoomIndex,
    cache: ActiveRoomSurfaceCache,
) -> Option<(
    &'static [CachedRoomCell],
    &'static [u16],
    &'static [WorldVertex],
    &'static [CachedRoomSurface],
)> {
    if !cache.ready || cache.vertex_count > MAX_CACHED_ROOM_VERTICES {
        return None;
    }
    unsafe {
        let resident_slot = ROOM_STREAM_SCHEDULER.resident_slot_for(index)?;
        let byte_count = ROOM_STREAM_SCHEDULER.resident_byte_count(resident_slot)?;
        let bytes = streamed_room_slot_bytes(resident_slot, byte_count)?;
        let view = streamed_room_chunk_view(bytes, index)?;
        if cache.cell_first != view.cells_offset
            || cache.cell_count != view.cell_count
            || cache.cell_vertex_first != view.cell_vertices_offset
            || cache.cell_vertex_count != view.cell_vertex_count
            || cache.vertex_first != view.vertices_offset
            || cache.vertex_count != view.vertex_count
            || cache.surface_first != view.surfaces_offset
            || cache.surface_count != view.surface_count
        {
            return None;
        }
        let cells = streamed_record_slice::<LevelCachedRoomCellRecord>(
            bytes,
            view.total_bytes,
            view.cells_offset,
            view.cell_count,
        )?;
        let cell_vertices = streamed_record_slice::<u16>(
            bytes,
            view.total_bytes,
            view.cell_vertices_offset,
            view.cell_vertex_count,
        )?;
        let vertices = streamed_record_slice::<LevelCachedRoomVertexRecord>(
            bytes,
            view.total_bytes,
            view.vertices_offset,
            view.vertex_count,
        )?;
        let surfaces = streamed_record_slice::<LevelCachedRoomSurfaceRecord>(
            bytes,
            view.total_bytes,
            view.surfaces_offset,
            view.surface_count,
        )?;
        Some((
            cached_room_cells_from_level_records(cells),
            cell_vertices,
            cached_room_vertices_from_level_records(vertices),
            cached_room_surfaces_from_level_records(surfaces),
        ))
    }
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_record_slice<T>(
    bytes: &'static [u8],
    total_bytes: usize,
    offset: usize,
    count: usize,
) -> Option<&'static [T]> {
    if !streamed_chunk_range_valid::<T>(total_bytes, offset, count) {
        return None;
    }
    let byte_count = count.checked_mul(core::mem::size_of::<T>())?;
    let slice = bytes.get(offset..offset + byte_count)?;
    Some(unsafe { core::slice::from_raw_parts(slice.as_ptr().cast::<T>(), count) })
}

fn active_surface_cache_failed(cache: ActiveRoomSurfaceCache) -> bool {
    !cache.ready && cache.status != ActiveRoomCacheStatus::Empty
}

fn room_origin_x(record: &LevelRoomRecord) -> i32 {
    record.origin_x.saturating_mul(record.sector_size)
}

fn room_origin_z(record: &LevelRoomRecord) -> i32 {
    record.origin_z.saturating_mul(record.sector_size)
}

#[derive(Copy, Clone)]
struct ActiveRoomView {
    position: RoomPoint,
    sin_yaw: i32,
    cos_yaw: i32,
    sin_pitch: i32,
    cos_pitch: i32,
}

impl ActiveRoomView {
    fn from_camera(camera: WorldCamera) -> Self {
        Self {
            position: RoomPoint::new(camera.position.x, camera.position.y, camera.position.z),
            sin_yaw: camera.sin_yaw.raw(),
            cos_yaw: camera.cos_yaw.raw(),
            sin_pitch: camera.sin_pitch.raw(),
            cos_pitch: camera.cos_pitch.raw(),
        }
    }
}

#[derive(Copy, Clone)]
struct PortalVisibilitySpace {
    room: RoomIndex,
    view: ActiveRoomView,
    camera_global: RoomPoint,
}

fn portal_visibility_space_for_view(
    current_index: RoomIndex,
    view: ActiveRoomView,
) -> PortalVisibilitySpace {
    let camera_global = local_to_global_room_point(current_index, view.position);
    PortalVisibilitySpace {
        room: current_index,
        view,
        camera_global,
    }
}

fn portal_visibility_view_keys(view: ActiveRoomView) -> (i16, i16, i16, i16) {
    (
        (view.sin_yaw / 64) as i16,
        (view.cos_yaw / 64) as i16,
        (view.sin_pitch / 64) as i16,
        (view.cos_pitch / 64) as i16,
    )
}

fn authored_room_for_chunk(index: RoomIndex) -> Option<u32> {
    chunk_record_for_room(index).map(|chunk| chunk.authored_room)
}

fn chunk_record_for_room(index: RoomIndex) -> Option<&'static LevelChunkRecord> {
    if let Some(chunk) = ROOM_CHUNKS.get(index.to_usize()) {
        if chunk.room == index {
            return Some(chunk);
        }
    }
    ROOM_CHUNKS.iter().find(|chunk| chunk.room == index)
}

fn chunk_overlaps_collision_window(
    chunk: LevelChunkRecord,
    current_record: &LevelRoomRecord,
    chunk_record: &LevelRoomRecord,
    anchor: RoomPoint,
    margin: i32,
) -> bool {
    let sector_size = chunk_record.sector_size.max(1);
    let x0 = room_origin_x(chunk_record).saturating_sub(room_origin_x(current_record));
    let z0 = room_origin_z(chunk_record).saturating_sub(room_origin_z(current_record));
    let x1 = x0.saturating_add((chunk.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((chunk.depth as i32).saturating_mul(sector_size));
    let margin = margin.max(0);
    anchor.x.saturating_add(margin) >= x0
        && anchor.x.saturating_sub(margin) < x1
        && anchor.z.saturating_add(margin) >= z0
        && anchor.z.saturating_sub(margin) < z1
}

fn rect_distance_sq(x: i32, z: i32, x0: i32, x1: i32, z0: i32, z1: i32) -> u32 {
    let dx = if x < x0 {
        x0.saturating_sub(x)
    } else if x > x1 {
        x.saturating_sub(x1)
    } else {
        0
    };
    let dz = if z < z0 {
        z0.saturating_sub(z)
    } else if z > z1 {
        z.saturating_sub(z1)
    } else {
        0
    };
    square_i32_to_u32_saturating(dx).saturating_add(square_i32_to_u32_saturating(dz))
}

fn square_i32_to_u32_saturating(value: i32) -> u32 {
    let value = value.unsigned_abs();
    if value > 65_535 {
        u32::MAX
    } else {
        value.saturating_mul(value)
    }
}

fn axis_moved_at_least(a: i32, b: i32, threshold: i32) -> bool {
    let threshold = threshold.max(0);
    if a >= b {
        a.saturating_sub(b) >= threshold
    } else {
        b.saturating_sub(a) >= threshold
    }
}

fn point_xz_axis_moved_at_least(a: RoomPoint, b: RoomPoint, threshold: i32) -> bool {
    axis_moved_at_least(a.x, b.x, threshold) || axis_moved_at_least(a.z, b.z, threshold)
}

fn point_xyz_axis_moved_at_least(a: RoomPoint, b: RoomPoint, threshold: i32) -> bool {
    axis_moved_at_least(a.x, b.x, threshold)
        || axis_moved_at_least(a.y, b.y, threshold)
        || axis_moved_at_least(a.z, b.z, threshold)
}

fn room_bounds(record: &LevelRoomRecord, room: RuntimeRoom<'_>) -> (i32, i32, i32, i32) {
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let x1 = x0.saturating_add((room.width() as i32).saturating_mul(record.sector_size));
    let z1 = z0.saturating_add((room.depth() as i32).saturating_mul(record.sector_size));
    (x0, x1, z0, z1)
}

fn collect_portal_room_bounds(out: &mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS]) -> usize {
    let mut count = 0usize;
    for visibility in ROOM_VISIBILITY {
        let Some(record) = ROOMS.get(visibility.room.to_usize()) else {
            continue;
        };
        let first = visibility.cell_first.to_usize();
        let end = first.saturating_add(visibility.cell_count as usize);
        let Some(cells) = VISIBILITY_CELLS.get(first..end) else {
            continue;
        };
        let sector_size = record.sector_size.max(1);
        let room_x0 = room_origin_x(record);
        let room_z0 = room_origin_z(record);
        for cell in cells {
            if cell.flags & visibility_cell_flags::HAS_GEOMETRY == 0 {
                continue;
            }
            let x0 = room_x0.saturating_add((cell.x as i32).saturating_mul(sector_size));
            let z0 = room_z0.saturating_add((cell.z as i32).saturating_mul(sector_size));
            count = push_portal_room_bounds(
                out,
                count,
                visibility.room,
                x0,
                x0.saturating_add(sector_size),
                z0,
                z0.saturating_add(sector_size),
            );
        }
    }
    if count > 0 {
        return count;
    }

    if !ROOM_CHUNKS.is_empty() {
        for chunk in ROOM_CHUNKS {
            let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
                continue;
            };
            let (x0, x1, z0, z1) = chunk_global_bounds(*chunk, record);
            count = push_portal_room_bounds(out, count, chunk.room, x0, x1, z0, z1);
        }
        return count;
    }

    for (raw_index, record) in ROOMS.iter().enumerate() {
        if raw_index >= u16::MAX as usize {
            break;
        }
        let Some(room) = parse_runtime_room(record) else {
            continue;
        };
        let (x0, x1, z0, z1) = room_bounds(record, room);
        count =
            push_portal_room_bounds(out, count, RoomIndex::new(raw_index as u16), x0, x1, z0, z1);
    }
    count
}

fn push_portal_room_bounds(
    out: &mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
    count: usize,
    room: RoomIndex,
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
) -> usize {
    if count >= out.len() || min_x >= max_x || min_z >= max_z {
        return count;
    }
    out[count] = PortalRoomBounds {
        room,
        min_x,
        max_x,
        min_y: PORTAL_ROOM_BOUNDS_MIN_Y,
        max_y: PORTAL_ROOM_BOUNDS_MAX_Y,
        min_z,
        max_z,
    };
    count + 1
}

fn collision_room_collected(
    collected_rooms: &[RoomIndex; MAX_COLLISION_ROOMS],
    count: usize,
    index: RoomIndex,
) -> bool {
    let mut i = 0usize;
    while i < count.min(collected_rooms.len()) {
        if collected_rooms[i] == index {
            return true;
        }
        i += 1;
    }
    false
}

fn room_index_containing_global(point: RoomPoint) -> Option<RoomIndex> {
    if !ROOM_CHUNKS.is_empty() {
        for chunk in ROOM_CHUNKS {
            let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
                continue;
            };
            if chunk_contains_global_point(*chunk, record, point) {
                return Some(chunk.room);
            }
        }
        return None;
    }
    for (raw_index, record) in ROOMS.iter().enumerate() {
        let Some(room) = parse_runtime_room(record) else {
            continue;
        };
        let (x0, x1, z0, z1) = room_bounds(record, room);
        if point.x >= x0 && point.x < x1 && point.z >= z0 && point.z < z1 {
            return Some(RoomIndex::new(raw_index as u16));
        }
    }
    None
}

fn room_index_containing_global_from(current: RoomIndex, point: RoomPoint) -> Option<RoomIndex> {
    if !ROOM_CHUNKS.is_empty() {
        let current_authored = authored_room_for_chunk(current);
        return room_index_containing_global_by_neighbours(current, point).or_else(|| {
            room_index_containing_global_in_authored(point, current_authored).or_else(|| {
                if current_authored.is_none() {
                    room_index_containing_global(point)
                } else {
                    None
                }
            })
        });
    }
    room_index_containing_global(point)
}

fn room_index_containing_global_by_neighbours(
    current: RoomIndex,
    point: RoomPoint,
) -> Option<RoomIndex> {
    let current_authored = authored_room_for_chunk(current);
    // Manual portal rooms can be L-shaped; topology comes from cells, not bboxes.
    let mut queue = [INVALID_ROOM_INDEX; MAX_PORTAL_ROOM_BOUNDS];
    let mut visited = [INVALID_ROOM_INDEX; MAX_PORTAL_ROOM_BOUNDS];
    let mut head = 0usize;
    let mut tail = 0usize;
    let mut visited_count = 0usize;
    push_room_search(
        current,
        &mut queue,
        &mut tail,
        &mut visited,
        &mut visited_count,
    );

    while head < tail {
        let index = queue[head];
        head += 1;
        if current_authored.is_some() && authored_room_for_chunk(index) != current_authored {
            continue;
        }
        let Some(chunk) = chunk_record_for_room(index) else {
            continue;
        };
        let Some(record) = ROOMS.get(index.to_usize()) else {
            continue;
        };
        if chunk_contains_global_point(*chunk, record, point) {
            return Some(index);
        }
        for neighbour in chunk_neighbours(*chunk) {
            push_room_search(
                neighbour,
                &mut queue,
                &mut tail,
                &mut visited,
                &mut visited_count,
            );
        }
    }
    None
}

fn room_index_containing_global_in_authored(
    point: RoomPoint,
    authored_room: Option<u32>,
) -> Option<RoomIndex> {
    for chunk in ROOM_CHUNKS {
        if authored_room.is_some() && Some(chunk.authored_room) != authored_room {
            continue;
        }
        let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
            continue;
        };
        if chunk_contains_global_point(*chunk, record, point) {
            return Some(chunk.room);
        }
    }
    None
}

fn push_room_search(
    room: RoomIndex,
    queue: &mut [RoomIndex; MAX_PORTAL_ROOM_BOUNDS],
    tail: &mut usize,
    visited: &mut [RoomIndex; MAX_PORTAL_ROOM_BOUNDS],
    visited_count: &mut usize,
) {
    if room == INVALID_ROOM_INDEX || *tail >= queue.len() || *visited_count >= visited.len() {
        return;
    }
    let mut i = 0usize;
    while i < *visited_count {
        if visited[i] == room {
            return;
        }
        i += 1;
    }
    visited[*visited_count] = room;
    *visited_count += 1;
    queue[*tail] = room;
    *tail += 1;
}

fn chunk_neighbours(chunk: LevelChunkRecord) -> [RoomIndex; 4] {
    [
        chunk.neighbours.north,
        chunk.neighbours.east,
        chunk.neighbours.south,
        chunk.neighbours.west,
    ]
}

fn chunk_contains_global_point(
    chunk: LevelChunkRecord,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> bool {
    if chunk.room.to_usize() >= ROOMS.len() {
        return false;
    }
    match room_visibility_contains_global_point(chunk.room, record, point) {
        Some(contains) => contains,
        None => chunk_bounds_contains_global_point(chunk, record, point),
    }
}

fn chunk_bounds_contains_global_point(
    chunk: LevelChunkRecord,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> bool {
    let (x0, x1, z0, z1) = chunk_global_bounds(chunk, record);
    point.x >= x0 && point.x < x1 && point.z >= z0 && point.z < z1
}

fn room_visibility_contains_global_point(
    room: RoomIndex,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> Option<bool> {
    let sector_size = record.sector_size.max(1);
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let local_x = point.x.checked_sub(x0)?;
    let local_z = point.z.checked_sub(z0)?;
    if local_x < 0 || local_z < 0 {
        return Some(false);
    }
    let sx_raw = local_x / sector_size;
    let sz_raw = local_z / sector_size;
    if sx_raw > u16::MAX as i32 || sz_raw > u16::MAX as i32 {
        return Some(false);
    }
    let sx = sx_raw as u16;
    let sz = sz_raw as u16;
    room_visibility_contains_cell(room, sx, sz)
}

fn room_visibility_contains_cell(room: RoomIndex, sx: u16, sz: u16) -> Option<bool> {
    let visibility = ROOM_VISIBILITY
        .iter()
        .find(|visibility| visibility.room == room)?;
    let first = visibility.cell_first.to_usize();
    let count = visibility.cell_count as usize;
    let cells = VISIBILITY_CELLS.get(first..first.checked_add(count)?)?;
    let mut i = 0usize;
    while i < cells.len() {
        let cell = cells[i];
        if cell.room == room && cell.x == sx && cell.z == sz {
            return Some(cell.flags & visibility_cell_flags::HAS_GEOMETRY != 0);
        }
        i += 1;
    }
    Some(false)
}

fn chunk_global_bounds(chunk: LevelChunkRecord, record: &LevelRoomRecord) -> (i32, i32, i32, i32) {
    let sector_size = record.sector_size.max(1);
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let x1 = x0.saturating_add((chunk.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((chunk.depth as i32).saturating_mul(sector_size));
    (x0, x1, z0, z1)
}

fn local_to_global_room_point(room: RoomIndex, point: RoomPoint) -> RoomPoint {
    let Some(record) = ROOMS.get(room.to_usize()) else {
        return point;
    };
    RoomPoint::new(
        point.x.saturating_add(room_origin_x(record)),
        point.y,
        point.z.saturating_add(room_origin_z(record)),
    )
}

fn global_to_local_room_point(room: RoomIndex, point: RoomPoint) -> RoomPoint {
    let Some(record) = ROOMS.get(room.to_usize()) else {
        return point;
    };
    RoomPoint::new(
        point.x.saturating_sub(room_origin_x(record)),
        point.y,
        point.z.saturating_sub(room_origin_z(record)),
    )
}

fn camera_for_room(camera: WorldCamera, active: ActiveRuntimeRoom) -> WorldCamera {
    WorldCamera::from_basis(
        camera.projection,
        WorldVertex::new(
            camera.position.x.saturating_sub(active.offset_x),
            camera.position.y,
            camera.position.z.saturating_sub(active.offset_z),
        ),
        camera.sin_yaw,
        camera.cos_yaw,
        camera.sin_pitch,
        camera.cos_pitch,
    )
}

fn active_room_overlaps_collision_window(
    active: ActiveRuntimeRoom,
    anchor: RoomPoint,
    margin: i32,
) -> bool {
    let sector_size = active.sector_size.max(1);
    let x0 = active.offset_x;
    let z0 = active.offset_z;
    let x1 = x0.saturating_add((active.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((active.depth as i32).saturating_mul(sector_size));
    let margin = margin.max(0);
    anchor.x.saturating_add(margin) >= x0
        && anchor.x.saturating_sub(margin) < x1
        && anchor.z.saturating_add(margin) >= z0
        && anchor.z.saturating_sub(margin) < z1
}

fn player_actor_depth_for_room(
    active: ActiveRuntimeRoom,
    character: Option<RuntimeCharacter>,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    player: RoomPoint,
    camera: &WorldCamera,
) -> Option<i32> {
    let character = character?;
    let runtime_model = models.get(character.model.to_usize()).copied().flatten()?;
    let origin = floor_anchored_model_origin(
        player.x.saturating_sub(active.offset_x),
        player.y,
        player.z.saturating_sub(active.offset_z),
        runtime_model.world_height,
    );
    Some(camera.view_vertex(origin).z)
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
            self.shade_tint_at_depth(
                vertices[0],
                material.texture.tint(),
                depths[0],
            ),
            self.shade_tint_at_depth(
                vertices[1],
                material.texture.tint(),
                depths[1],
            ),
            self.shade_tint_at_depth(
                vertices[2],
                material.texture.tint(),
                depths[2],
            ),
            self.shade_tint_at_depth(
                vertices[3],
                material.texture.tint(),
                depths[3],
            ),
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

fn upload_shadow_texture() -> Option<TextureMaterial> {
    let texture = Texture::from_bytes(SHADOW_CIRCLE_BLOB).ok()?;
    if texture.width() != 64 || texture.height() != 64 || texture.clut_entries() != 16 {
        return None;
    }

    upload_bytes(
        VramRect::new(
            SHADOW_TEXTURE_X,
            SHADOW_TPAGE.y(),
            texture.halfwords_per_row(),
            texture.height(),
        ),
        texture.pixel_bytes(),
    );
    upload_clut(
        VramRect::new(SHADOW_CLUT.x(), SHADOW_CLUT.y(), texture.clut_entries(), 1),
        texture.clut_bytes(),
    );

    Some(
        TextureMaterial::blended(
            SHADOW_CLUT.uv_clut_word(),
            SHADOW_TPAGE.uv_tpage_word(0),
            (0x80, 0x80, 0x80),
            BlendMode::Average,
        )
        .with_raw_texture(true),
    )
}

fn upload_particle_texture() -> TextureMaterial {
    let mut pixels =
        [0u8; (PARTICLE_TEXTURE_HALFWORDS_PER_ROW as usize) * (PARTICLE_TEXTURE_SIZE as usize) * 2];
    let mut row = 0usize;
    while row < PARTICLE_TEXTURE_SIZE as usize {
        let mut col = 0usize;
        while col < PARTICLE_TEXTURE_SIZE as usize {
            let dx = (col as i32 * 2 + 1) - PARTICLE_TEXTURE_SIZE as i32;
            let dy = (row as i32 * 2 + 1) - PARTICLE_TEXTURE_SIZE as i32;
            let inside = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)) <= 225;
            if inside {
                let halfword = row * PARTICLE_TEXTURE_HALFWORDS_PER_ROW as usize + (col / 4);
                let shift = (col & 3) * 4;
                let raw = u16::from_le_bytes([pixels[halfword * 2], pixels[halfword * 2 + 1]])
                    | (1u16 << shift);
                let packed = raw.to_le_bytes();
                pixels[halfword * 2] = packed[0];
                pixels[halfword * 2 + 1] = packed[1];
            }
            col += 1;
        }
        row += 1;
    }

    let mut clut = [0u8; 32];
    let white = 0x7FFFu16.to_le_bytes();
    clut[2] = white[0];
    clut[3] = white[1];

    upload_bytes(
        VramRect::new(
            PARTICLE_TEXTURE_X,
            PARTICLE_TPAGE.y(),
            PARTICLE_TEXTURE_HALFWORDS_PER_ROW,
            PARTICLE_TEXTURE_SIZE,
        ),
        &pixels,
    );
    upload_clut(
        VramRect::new(PARTICLE_CLUT.x(), PARTICLE_CLUT.y(), 16, 1),
        &clut,
    );

    TextureMaterial::blended(
        PARTICLE_CLUT.uv_clut_word(),
        PARTICLE_TPAGE.uv_tpage_word(0),
        (0x80, 0x80, 0x80),
        BlendMode::Average,
    )
}

/// Upload `asset_bytes` to VRAM if not already resident; return
/// the slot record so the caller can build a TextureMaterial.
/// Returns `None` if the texture parse fails or the VRAM table
/// is full.
/// Look up the VRAM slot a previously-uploaded asset occupies.
/// VRAM_SLOTS is the source of truth -- `RESIDENCY` only tracks
/// the *contract*, which is pre-marked by `ensure_room_resident`
/// before any actual upload runs.
fn find_vram_slot(asset_id: AssetId, clut_mode: VramSlotClutMode) -> Option<VramSlot> {
    unsafe {
        VRAM_SLOTS
            .iter()
            .filter_map(|s| *s)
            .find(|s| s.ready && s.asset == asset_id && s.clut_mode == clut_mode)
    }
}

fn find_room_texture_vram_slot(asset_id: AssetId) -> Option<VramSlot> {
    unsafe {
        VRAM_SLOTS.iter().filter_map(|s| *s).find(|s| {
            s.ready
                && s.asset == asset_id
                && matches!(
                    s.clut_mode,
                    VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
                )
        })
    }
}

fn pending_vram_upload(asset_id: AssetId, clut_mode: VramSlotClutMode) -> bool {
    unsafe {
        VRAM_SLOTS
            .iter()
            .filter_map(|s| *s)
            .any(|s| !s.ready && s.asset == asset_id && s.clut_mode == clut_mode)
            || VRAM_UPLOAD_QUEUE.contains(asset_id, clut_mode)
    }
}

fn pending_room_texture_upload(asset_id: AssetId) -> bool {
    unsafe {
        VRAM_SLOTS.iter().filter_map(|s| *s).any(|s| {
            !s.ready
                && s.asset == asset_id
                && matches!(
                    s.clut_mode,
                    VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
                )
        })
    }
}

unsafe fn mark_vram_slot_ready(index: usize) {
    let Some(mut slot) = VRAM_SLOTS.get(index).copied().flatten() else {
        return;
    };
    slot.ready = true;
    VRAM_SLOTS[index] = Some(slot);
    let _ = RESIDENCY.mark_vram_resident(slot.asset);
}

fn ensure_texture_uploaded(asset_id: AssetId, asset_bytes: &'static [u8]) -> Option<VramSlot> {
    let texture = Texture::from_bytes(asset_bytes).ok()?;
    let clut_mode = if texture.index_zero_transparent() {
        VramSlotClutMode::TransparentZero
    } else {
        VramSlotClutMode::OpaqueZero
    };
    ensure_texture_uploaded_with_clut_mode(asset_id, asset_bytes, clut_mode)
}

fn ensure_texture_uploaded_with_clut_mode(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
    clut_mode: VramSlotClutMode,
) -> Option<VramSlot> {
    // VRAM_SLOTS is the source of truth for "have we actually
    // uploaded this asset". `RESIDENCY` is the *contract* -- it's
    // pre-marked by `ensure_room_resident` before any upload runs,
    // so reading it here would falsely report assets as uploaded
    // and skip the upload entirely.
    if let Some(slot) = find_vram_slot(asset_id, clut_mode) {
        return Some(slot);
    }
    if pending_vram_upload(asset_id, clut_mode) {
        return None;
    }
    if pending_room_texture_upload(asset_id) {
        return None;
    }
    unsafe {
        if !VRAM_UPLOAD_QUEUE.has_free_slot() {
            return None;
        }
    }

    let texture = Texture::from_bytes(asset_bytes).ok()?;
    if texture.clut_entries() != 16 {
        return None;
    }

    // Capacity check before we touch any VRAM state.
    let count = unsafe { VRAM_SLOT_COUNT };
    let room_count = unsafe { ROOM_TEXTURE_COUNT };
    if count >= MAX_RESIDENT_VRAM_ASSETS {
        return None;
    }

    if texture.width() > ROOM_TILE_TEXELS || texture.height() > ROOM_TILE_TEXELS {
        return None;
    }

    let texture_width = room_texture_window_size(texture.width())?;
    let texture_height = room_texture_window_size(texture.height())?;
    let texture_width_halfwords = u16::from(texture_width) / 4;
    let texture_height_rows = u16::from(texture_height);
    if texture.halfwords_per_row() > texture_width_halfwords
        || texture.height() > texture_height_rows
    {
        return None;
    }
    let src_bytes = texture.pixel_bytes();
    let src_len = (texture.halfwords_per_row() as usize)
        .saturating_mul(texture.height() as usize)
        .saturating_mul(2);
    if src_bytes.len() != src_len {
        return None;
    }

    let room_index = u16::try_from(room_count).ok()?;
    let clut_x = ROOM_CLUT_BASE_X.checked_add(room_index.checked_mul(ROOM_CLUT_STRIDE)?)?;
    if clut_x.checked_add(texture.clut_entries())? > 1024 {
        return None;
    }

    if let Some(shared_texture) = find_room_texture_vram_slot(asset_id) {
        let slot = VramSlot {
            asset: asset_id,
            clut_mode,
            ready: false,
            clut_word: Clut::new(clut_x, ROOM_CLUT_Y).uv_clut_word(),
            tpage_word: shared_texture.tpage_word,
            texture_window: shared_texture.texture_window,
            texture_width: shared_texture.texture_width,
            texture_height: shared_texture.texture_height,
        };
        unsafe {
            VRAM_SLOTS[count] = Some(slot);
            VRAM_SLOT_COUNT = count + 1;
            ROOM_TEXTURE_COUNT = room_count + 1;
            if !VRAM_UPLOAD_QUEUE.push(VramUploadJob {
                active: true,
                slot_index: count as u16,
                asset: asset_id,
                clut_mode,
                kind: VramUploadKind::ClutOnly,
                bytes: Some(asset_bytes),
                texture_x: 0,
                texture_y: 0,
                texture_width_halfwords: 0,
                texture_height_rows: 0,
                next_texture_row: 0,
                clut_x,
                clut_y: ROOM_CLUT_Y,
                clut_entries: texture.clut_entries(),
                clut_uploaded: false,
            }) {
                VRAM_SLOTS[count] = None;
                VRAM_SLOT_COUNT = count;
                ROOM_TEXTURE_COUNT = room_count;
                return None;
            }
        }
        return None;
    }

    // Pack room materials on the GP0(E2) 8-texel grid inside 4bpp
    // tpages. A 32x32 texture now consumes a 32x32 window instead of
    // burning a whole old 64x64 cell.
    let placement = unsafe {
        ROOM_TEXTURE_ALLOCATOR.allocate(u16::from(texture_width), u16::from(texture_height))?
    };
    let page_index = placement.page_index();
    let tpage_x = ROOM_TPAGE_BASE_X.checked_add(page_index.checked_mul(ROOM_TPAGE_STRIDE_HW)?)?;
    let end_x = tpage_x.checked_add(ROOM_TPAGE_STRIDE_HW)?;
    if tpage_x % 64 != 0 || end_x > ROOM_TPAGE_LIMIT_X {
        return None;
    }
    let tpage = Tpage::new(tpage_x, SHARED_TPAGE.y(), TexDepth::Bit4);
    let texture_x = tpage_x.checked_add(u16::from(placement.origin_u()) / 4)?;
    let texture_y = SHARED_TPAGE
        .y()
        .checked_add(u16::from(placement.origin_v()))?;

    let clut = Clut::new(clut_x, ROOM_CLUT_Y);
    let slot = VramSlot {
        asset: asset_id,
        clut_mode,
        ready: false,
        clut_word: clut.uv_clut_word(),
        tpage_word: tpage.uv_tpage_word(0),
        texture_window: TextureWindow::power_of_two_tile(
            placement.origin_u(),
            placement.origin_v(),
            texture_width,
            texture_height,
        ),
        texture_width: u16::from(texture_width),
        texture_height: u16::from(texture_height),
    };

    unsafe {
        VRAM_SLOTS[count] = Some(slot);
        VRAM_SLOT_COUNT = count + 1;
        ROOM_TEXTURE_COUNT = room_count + 1;
        if !VRAM_UPLOAD_QUEUE.push(VramUploadJob {
            active: true,
            slot_index: count as u16,
            asset: asset_id,
            clut_mode,
            kind: VramUploadKind::TextureAndClut,
            bytes: Some(asset_bytes),
            texture_x,
            texture_y,
            texture_width_halfwords,
            texture_height_rows,
            next_texture_row: 0,
            clut_x,
            clut_y: ROOM_CLUT_Y,
            clut_entries: texture.clut_entries(),
            clut_uploaded: false,
        }) {
            VRAM_SLOTS[count] = None;
            VRAM_SLOT_COUNT = count;
            ROOM_TEXTURE_COUNT = room_count;
            return None;
        }
    }

    None
}

fn room_texture_window_size(size: u16) -> Option<u8> {
    if size < 8 || size > ROOM_TILE_TEXELS || !size.is_power_of_two() || size % 8 != 0 {
        return None;
    }
    u8::try_from(size).ok()
}

fn ensure_sky_panorama_uploaded(asset_id: AssetId, asset_bytes: &[u8]) -> Option<VramSlot> {
    if let Some(slot) = find_vram_slot(asset_id, VramSlotClutMode::SkyPanorama) {
        return Some(slot);
    }
    let texture = Texture::from_bytes(asset_bytes).ok()?;
    if texture.clut_entries() != SKY_PANORAMA_CLUT_ENTRIES * SKY_PANORAMA_PALETTE_BANDS as u16
        || texture.width() != SKY_PANORAMA_WIDTH
        || texture.height() != SKY_PANORAMA_HEIGHT
        || texture.halfwords_per_row() != SKY_PANORAMA_WIDTH / 4
    {
        return None;
    }
    let count = unsafe { VRAM_SLOT_COUNT };
    if count >= MAX_RESIDENT_VRAM_ASSETS {
        return None;
    }
    let expected_pixel_bytes = (texture.halfwords_per_row() as usize)
        .saturating_mul(texture.height() as usize)
        .saturating_mul(2);
    if texture.pixel_bytes().len() != expected_pixel_bytes {
        return None;
    }

    telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
    telemetry::counter(telemetry::counter::ROOM_TEXTURE_UPLOADS, 1);
    upload_bytes(
        VramRect::new(
            SKY_PANORAMA_LEFT_TPAGE.x(),
            SKY_PANORAMA_LEFT_TPAGE.y(),
            texture.halfwords_per_row(),
            texture.height(),
        ),
        texture.pixel_bytes(),
    );
    let clut_row_bytes = usize::from(SKY_PANORAMA_CLUT_ENTRIES) * 2;
    if texture.clut_bytes().len() != clut_row_bytes * SKY_PANORAMA_PALETTE_BANDS {
        telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);
        return None;
    }
    for band in 0..SKY_PANORAMA_PALETTE_BANDS {
        let offset = band * clut_row_bytes;
        upload_model_clut(
            VramRect::new(
                SKY_PANORAMA_CLUT_X,
                SKY_PANORAMA_CLUT_Y + band as u16,
                SKY_PANORAMA_CLUT_ENTRIES,
                1,
            ),
            &texture.clut_bytes()[offset..offset + clut_row_bytes],
            texture.index_zero_transparent(),
        );
    }
    telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);

    let slot = VramSlot {
        asset: asset_id,
        clut_mode: VramSlotClutMode::SkyPanorama,
        ready: true,
        clut_word: sky_panorama_clut_word(0),
        tpage_word: SKY_PANORAMA_LEFT_TPAGE.uv_tpage_word(0),
        texture_window: TextureWindow::NONE,
        texture_width: texture.width(),
        texture_height: texture.height(),
    };
    unsafe {
        VRAM_SLOTS[count] = Some(slot);
        VRAM_SLOT_COUNT = count + 1;
        let _ = RESIDENCY.mark_vram_resident(asset_id);
    }
    Some(slot)
}

/// Upload an 8bpp model atlas to the dedicated model VRAM
/// region. Returns a `VramSlot` carrying the 8bpp tpage word
/// and the atlas's CLUT word. Reuses an existing slot when the
/// asset's already resident.
///
/// Caller is responsible for confirming `asset_bytes` parses as
/// a `Texture` whose CLUT carries 256 entries (8bpp). Anything
/// else returns `None`.
fn ensure_model_atlas_uploaded(asset_id: AssetId, asset_bytes: &[u8]) -> Option<VramSlot> {
    // Same caveat as `ensure_texture_uploaded`: VRAM_SLOTS is
    // the source of truth, not the residency tracker.
    if let Some(slot) = find_vram_slot(asset_id, VramSlotClutMode::ModelAtlas) {
        return Some(slot);
    }
    let texture = Texture::from_bytes(asset_bytes).ok()?;
    if texture.clut_entries() != 256 {
        // Only 8bpp atlases supported -- 4bpp model atlases
        // would round-trip through `ensure_texture_uploaded`.
        return None;
    }

    let count = unsafe { VRAM_SLOT_COUNT };
    let atlas_count = unsafe { MODEL_ATLAS_COUNT };
    if count >= MAX_RESIDENT_VRAM_ASSETS {
        return None;
    }
    let texture_width = texture.width();
    let texture_height = texture.height();
    let texture_halfwords_per_row = texture.halfwords_per_row();
    if texture_width == 0
        || texture_width > 256
        || texture_height == 0
        || texture_height > 256
        || texture_halfwords_per_row > MODEL_TPAGE_MAX_HALFWORDS
    {
        return None;
    }
    let expected_pixel_bytes = (texture_halfwords_per_row as usize)
        .saturating_mul(texture_height as usize)
        .saturating_mul(2);
    if texture.pixel_bytes().len() != expected_pixel_bytes {
        return None;
    }

    let tpage_x = MODEL_TPAGE.x() + unsafe { MODEL_TPAGE_X_CURSOR };
    let slot_halfwords = if texture_halfwords_per_row <= MODEL_TPAGE_SLOT_HALFWORDS {
        MODEL_TPAGE_SLOT_HALFWORDS
    } else {
        MODEL_TPAGE_MAX_HALFWORDS
    };
    if tpage_x % 64 != 0 || tpage_x.checked_add(slot_halfwords)? > MODEL_TPAGE_LIMIT_X {
        return None;
    }
    telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
    telemetry::counter(telemetry::counter::MODEL_ATLAS_UPLOADS, 1);
    let pix_rect = VramRect::new(
        tpage_x,
        MODEL_TPAGE.y(),
        texture_halfwords_per_row,
        texture_height,
    );
    upload_bytes(pix_rect, texture.pixel_bytes());
    let tpage = Tpage::new(tpage_x, MODEL_TPAGE.y(), TexDepth::Bit8);

    // 256-entry CLUT: 256 halfwords on a single row.
    let clut_y = MODEL_CLUT_BASE_Y + atlas_count as u16;
    let clut_rect = VramRect::new(0, clut_y, texture.clut_entries(), 1);
    upload_model_clut(
        clut_rect,
        texture.clut_bytes(),
        texture.index_zero_transparent(),
    );
    telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);

    let slot = VramSlot {
        asset: asset_id,
        clut_mode: VramSlotClutMode::ModelAtlas,
        ready: true,
        clut_word: Clut::new(0, clut_y).uv_clut_word(),
        tpage_word: tpage.uv_tpage_word(0),
        texture_window: TextureWindow::NONE,
        texture_width,
        texture_height,
    };

    unsafe {
        VRAM_SLOTS[count] = Some(slot);
        VRAM_SLOT_COUNT = count + 1;
        MODEL_TPAGE_X_CURSOR += slot_halfwords;
        MODEL_ATLAS_COUNT = atlas_count + 1;
        let _ = RESIDENCY.mark_vram_resident(asset_id);
    }
    Some(slot)
}

/// Animate + render placed model instances whose owning room matches
/// `current_room`. Meshes, clips, and atlas materials are resolved by
/// `load_runtime_models` once at init; the frame path only chooses
/// phase + transform and submits packets.
///
/// Errors (parse failure, missing asset) skip the instance
/// rather than crashing.
#[derive(Copy, Clone, Debug, Default)]
struct ModelInstanceDrawStats {
    draws: u16,
    bounds_tests: u16,
    bounds_culled: u16,
    stats: TexturedModelRenderStats,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ModelInstanceDepthPass {
    All,
    BehindPlayer(i32),
    InFrontOfPlayer(i32),
}

impl ModelInstanceDepthPass {
    fn includes(self, depth: i32) -> bool {
        match self {
            Self::All => true,
            Self::BehindPlayer(player_depth) => depth >= player_depth,
            Self::InFrontOfPlayer(player_depth) => depth < player_depth,
        }
    }
}

fn accumulate_model_instance_draw_stats(
    total: &mut ModelInstanceDrawStats,
    stats: ModelInstanceDrawStats,
) {
    total.draws = total.draws.saturating_add(stats.draws);
    total.bounds_tests = total.bounds_tests.saturating_add(stats.bounds_tests);
    total.bounds_culled = total.bounds_culled.saturating_add(stats.bounds_culled);
    accumulate_model_stats(&mut total.stats, stats.stats);
}

fn draw_model_instance_shadows(
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let mut drawn = 0usize;
    for inst in MODEL_INSTANCES {
        if inst.room != current_room || drawn >= MAX_MODEL_INSTANCES {
            continue;
        }
        let Some(runtime_model) = models.get(inst.model.to_usize()).copied().flatten() else {
            continue;
        };

        draw_actor_shadow(
            inst.x,
            inst.y,
            inst.z,
            actor_shadow_radius(i32::from(runtime_model.collision_radius)),
            camera,
            options,
            material,
            triangles,
            world,
        );
        drawn += 1;
    }
}

fn draw_actor_shadow(
    x: i32,
    floor_y: i32,
    z: i32,
    radius: i32,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    if radius <= 0 {
        return;
    }
    let y = floor_y.saturating_add(SHADOW_FLOOR_LIFT);
    let h = radius;
    let verts = [
        WorldVertex::new(x.saturating_sub(h), y, z.saturating_sub(h)),
        WorldVertex::new(x.saturating_add(h), y, z.saturating_sub(h)),
        WorldVertex::new(x.saturating_add(h), y, z.saturating_add(h)),
        WorldVertex::new(x.saturating_sub(h), y, z.saturating_add(h)),
    ];
    let shadow_options = options
        .with_depth_policy(DepthPolicy::Nearest)
        .with_depth_bias(SHADOW_DEPTH_BIAS.saturating_neg())
        .with_cull_mode(CullMode::None)
        .with_material_layer(material);
    const UVS: [(u8, u8); 4] = [
        (SHADOW_TEXEL_U, 0),
        (SHADOW_UV_MAX, 0),
        (SHADOW_UV_MAX, 63),
        (SHADOW_TEXEL_U, 63),
    ];
    let _ =
        world.submit_textured_world_quad(triangles, *camera, verts, UVS, material, shadow_options);
}

fn actor_shadow_radius(base_radius: i32) -> i32 {
    base_radius
        .saturating_mul(SHADOW_RADIUS_SCALE_NUM)
        .checked_div(SHADOW_RADIUS_SCALE_DEN)
        .unwrap_or(base_radius)
        .clamp(SHADOW_RADIUS_MIN, SHADOW_RADIUS_MAX)
}

fn draw_collision_cylinder_debug(
    position: RoomPoint,
    radius: i32,
    height: i32,
    camera: WorldCamera,
    color: (u8, u8, u8),
) {
    if radius <= 0 || height <= 0 {
        return;
    }

    let bottom_y = position.y.saturating_add(COLLISION_DEBUG_FLOOR_LIFT);
    let top_y = position
        .y
        .saturating_add(height.max(COLLISION_DEBUG_FLOOR_LIFT));
    let mut bottom = [None; COLLISION_DEBUG_SEGMENTS];
    let mut top = [None; COLLISION_DEBUG_SEGMENTS];
    let mut i = 0usize;
    while i < COLLISION_DEBUG_SEGMENTS {
        let (dx, dz) = collision_debug_ring_offset(radius, i);
        let x = position.x.saturating_add(dx);
        let z = position.z.saturating_add(dz);
        bottom[i] = camera
            .project_world(WorldVertex::new(x, bottom_y, z))
            .map(screen_xy);
        top[i] = camera
            .project_world(WorldVertex::new(x, top_y, z))
            .map(screen_xy);
        i += 1;
    }

    i = 0;
    while i < COLLISION_DEBUG_SEGMENTS {
        let next = (i + 1) % COLLISION_DEBUG_SEGMENTS;
        draw_optional_debug_line(bottom[i], bottom[next], color);
        draw_optional_debug_line(top[i], top[next], color);
        if i % 2 == 0 {
            draw_optional_debug_line(bottom[i], top[i], color);
        }
        i += 1;
    }
}

fn collision_debug_ring_offset(radius: i32, index: usize) -> (i32, i32) {
    let diagonal = radius.saturating_mul(181) / 256;
    match index & 7 {
        0 => (radius, 0),
        1 => (diagonal, diagonal),
        2 => (0, radius),
        3 => (diagonal.saturating_neg(), diagonal),
        4 => (radius.saturating_neg(), 0),
        5 => (diagonal.saturating_neg(), diagonal.saturating_neg()),
        6 => (0, radius.saturating_neg()),
        _ => (diagonal, diagonal.saturating_neg()),
    }
}

fn screen_xy(vertex: ProjectedVertex) -> (i16, i16) {
    (vertex.sx, vertex.sy)
}

fn draw_optional_debug_line(a: Option<(i16, i16)>, b: Option<(i16, i16)>, color: (u8, u8, u8)) {
    let (Some(a), Some(b)) = (a, b) else {
        return;
    };
    draw_line_mono(a.0, a.1, b.0, b.1, color.0, color.1, color.2);
}

fn draw_model_instances(
    current_room: RoomIndex,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    depth_pass: ModelInstanceDepthPass,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> ModelInstanceDrawStats {
    let mut drawn = 0usize;
    let mut out = ModelInstanceDrawStats::default();
    for inst in MODEL_INSTANCES {
        if inst.room != current_room || drawn >= MAX_MODEL_INSTANCES {
            continue;
        }
        let Some(runtime_model) = models.get(inst.model.to_usize()).copied().flatten() else {
            continue;
        };

        // Clip resolution: per-instance override → model default.
        // The cooker validates that both end up `< clip_count`,
        // so by the time we get here `clip_local` is in-range.
        let clip_local = inst.clip.unwrap_or(runtime_model.default_clip);
        let Some(anim) = runtime_model.clip(clips, clip_local) else {
            continue;
        };
        let phase = anim.phase_at_tick_q12(elapsed_tick.as_u32(), video_hz.as_u16());
        let bounds = model_frame_bounds(runtime_model, clip_local, phase);
        let clip_anchor = model_clip_anchor(runtime_model, clip_local);
        let reference_anchor = model_clip_anchor(runtime_model, runtime_model.default_clip);
        let pose_translation =
            model_pose_anchor_translation(anim, phase, clip_anchor, reference_anchor, None);

        // Instance Y-axis rotation from authored yaw. PSX angle
        // units (4096 per turn) → Q12 sin/cos via the existing
        // GTE shim, then composed into a rotation matrix.
        let root_yaw = Angle::from_q12(inst.yaw as u16);
        let model_rotation = yaw_rotation_matrix(root_yaw.add_signed_q12(inst.visual_yaw));
        // Authored instance positions are floor anchors; cooked
        // model vertices are centred around their bounds.
        let origin = visual_model_origin(
            inst.x,
            inst.y,
            inst.z,
            runtime_model.world_height,
            inst.visual_offset,
            inst.visual_scale_q8,
            &model_rotation,
        );
        let local_to_world = visual_model_local_to_world(runtime_model, inst.visual_scale_q8);
        let bounds_origin =
            model_pose_translated_origin(origin, model_rotation, local_to_world, pose_translation);
        if !depth_pass.includes(camera.view_vertex(origin).z) {
            continue;
        }
        telemetry::stage_begin(telemetry::stage::MODEL_BOUNDS);
        out.bounds_tests = out.bounds_tests.saturating_add(1);
        let visible = match bounds {
            Some(bounds) if MODEL_BOUNDS_CULLING_ENABLED => model_bounds_visible(
                camera,
                options,
                bounds_origin,
                model_rotation,
                bounds,
                inst.visual_scale_q8,
            ),
            None => true,
            _ => true,
        };
        telemetry::stage_end(telemetry::stage::MODEL_BOUNDS);
        if !visible {
            out.bounds_culled = out.bounds_culled.saturating_add(1);
            continue;
        }

        let material = lighting.shade_model_material(origin, runtime_model.material);
        let model_options = options
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(CullMode::Back)
            .with_material_layer(material)
            .with_textured_triangle_splitting(true)
            .with_textured_triangle_max_edge(MODEL_TEXTURE_SPLIT_MAX_EDGE);

        telemetry::stage_begin(telemetry::stage::MODEL_DRAW);
        let faces = runtime_model_faces(runtime_model, model_faces);
        let stats = submit_runtime_model_predecoded(
            world,
            triangles,
            runtime_model,
            anim,
            phase,
            *camera,
            origin,
            model_rotation,
            local_to_world,
            pose_translation,
            material,
            model_options,
            faces,
            model_parts,
            model_vertices,
        );
        telemetry::stage_end(telemetry::stage::MODEL_DRAW);
        accumulate_model_stats(&mut out.stats, stats);
        if stats.primitive_overflow || stats.command_overflow {
            out.draws = drawn as u16;
            return out;
        }
        drawn += 1;
        out.draws = drawn as u16;
    }
    out
}

fn accumulate_model_stats(total: &mut TexturedModelRenderStats, next: TexturedModelRenderStats) {
    total.projected_vertices = total
        .projected_vertices
        .saturating_add(next.projected_vertices);
    total.submitted_triangles = total
        .submitted_triangles
        .saturating_add(next.submitted_triangles);
    total.culled_triangles = total.culled_triangles.saturating_add(next.culled_triangles);
    total.split_triangles = total.split_triangles.saturating_add(next.split_triangles);
    total.skipped_triangles = total
        .skipped_triangles
        .saturating_add(next.skipped_triangles);
    total.dropped_triangles = total
        .dropped_triangles
        .saturating_add(next.dropped_triangles);
    total.cpu_blended_vertices = total
        .cpu_blended_vertices
        .saturating_add(next.cpu_blended_vertices);
    total.packed_face_calls = total
        .packed_face_calls
        .saturating_add(next.packed_face_calls);
    total.packed_unclamped_face_calls = total
        .packed_unclamped_face_calls
        .saturating_add(next.packed_unclamped_face_calls);
    total.packed_clamped_face_calls = total
        .packed_clamped_face_calls
        .saturating_add(next.packed_clamped_face_calls);
    total.packed_general_face_calls = total
        .packed_general_face_calls
        .saturating_add(next.packed_general_face_calls);
    total.fallback_face_calls = total
        .fallback_face_calls
        .saturating_add(next.fallback_face_calls);
    total.hw_extent_fallbacks = total
        .hw_extent_fallbacks
        .saturating_add(next.hw_extent_fallbacks);
    total.near_plane_dropped_faces = total
        .near_plane_dropped_faces
        .saturating_add(next.near_plane_dropped_faces);
    total.hw_unsafe_dropped_faces = total
        .hw_unsafe_dropped_faces
        .saturating_add(next.hw_unsafe_dropped_faces);
    total.fast_submitted_triangles = total
        .fast_submitted_triangles
        .saturating_add(next.fast_submitted_triangles);
    total.vertex_overflow |= next.vertex_overflow;
    total.primitive_overflow |= next.primitive_overflow;
    total.command_overflow |= next.command_overflow;
}

/// Rotation matrix around the world Y axis.
fn yaw_rotation_matrix(yaw: Angle) -> Mat3I16 {
    let s = clamp_i16(yaw.sin().raw());
    let c = clamp_i16(yaw.cos().raw());
    Mat3I16 {
        m: [[c, 0, s], [0, 0x1000, 0], [-s, 0, c]],
    }
}

fn visual_model_local_to_world(
    runtime_model: RuntimeModelAsset,
    visual_scale_q8: u16,
) -> LocalToWorldScale {
    let scale_q8 = visual_scale_q8.max(1) as u32;
    let q12 = ((runtime_model.local_to_world.q12() as u32)
        .saturating_mul(scale_q8)
        .saturating_add((MODEL_VISUAL_SCALE_ONE_Q8 / 2) as u32))
        / MODEL_VISUAL_SCALE_ONE_Q8 as u32;
    LocalToWorldScale::from_q12(q12.clamp(1, u16::MAX as u32) as u16)
}

fn visual_model_origin(
    x: i32,
    y: i32,
    z: i32,
    world_height: u16,
    visual_offset: [i16; 3],
    _visual_scale_q8: u16,
    rotation: &Mat3I16,
) -> WorldVertex {
    let origin = floor_anchored_model_origin(x, y, z, world_height);
    let offset = rotate_offset_q12(
        rotation,
        [
            visual_offset[0] as i32,
            visual_offset[1] as i32,
            visual_offset[2] as i32,
        ],
    );
    WorldVertex::new(
        origin.x.saturating_add(offset[0]),
        origin.y.saturating_add(offset[1]),
        origin.z.saturating_add(offset[2]),
    )
}

fn animation_phase_at_tick_q12(
    animation: Animation<'static>,
    local_tick: u32,
    video_hz: VideoHz,
    looping: bool,
) -> u32 {
    let phase = animation.phase_at_tick_q12(local_tick, video_hz.as_u16());
    if looping {
        return phase;
    }
    let final_unique_frame = animation.frame_count().saturating_sub(2) as u32;
    phase.min(final_unique_frame << 12)
}

fn model_pose_anchor_translation(
    animation: Animation<'static>,
    phase_q12: u32,
    clip_anchor: Option<ModelClipAnchor>,
    reference_anchor: Option<ModelClipAnchor>,
    in_place_override: Option<bool>,
) -> ModelPoseTranslation {
    let Some(clip_anchor) = clip_anchor else {
        return ModelPoseTranslation::ZERO;
    };
    let reference_floor_y = reference_anchor.map(|anchor| anchor.floor_y);
    let in_place = in_place_override.unwrap_or(clip_anchor.in_place);
    let root_translation = if in_place {
        match (
            animation.pose(0, 0),
            animation.pose_looped_q12(phase_q12, 0),
        ) {
            (Some(first_root), Some(current_root)) => [
                first_root
                    .translation
                    .x
                    .saturating_sub(current_root.translation.x),
                0,
                first_root
                    .translation
                    .z
                    .saturating_sub(current_root.translation.z),
            ],
            _ => [0, 0, 0],
        }
    } else {
        [0, 0, 0]
    };
    let floor_y = match reference_floor_y {
        Some(reference_floor_y) => reference_floor_y.saturating_sub(clip_anchor.floor_y),
        None => 0,
    };
    ModelPoseTranslation {
        x: root_translation[0].saturating_add(clip_anchor.pose_offset[0]),
        y: root_translation[1]
            .saturating_add(floor_y)
            .saturating_add(clip_anchor.pose_offset[1]),
        z: root_translation[2].saturating_add(clip_anchor.pose_offset[2]),
    }
}

fn model_pose_translated_origin(
    origin: WorldVertex,
    rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    pose_translation: ModelPoseTranslation,
) -> WorldVertex {
    let scaled = [
        local_to_world.apply(pose_translation.x),
        local_to_world.apply(pose_translation.y),
        local_to_world.apply(pose_translation.z),
    ];
    let offset = rotate_offset_q12(&rotation, scaled);
    WorldVertex::new(
        origin.x.saturating_add(offset[0]),
        origin.y.saturating_add(offset[1]),
        origin.z.saturating_add(offset[2]),
    )
}

fn floor_anchored_model_origin(x: i32, y: i32, z: i32, world_height: u16) -> WorldVertex {
    WorldVertex::new(
        x,
        y.saturating_add(model_origin_floor_lift(world_height)),
        z,
    )
}

fn model_origin_floor_lift(world_height: u16) -> i32 {
    (world_height as i32) / 2
}

const MODEL_BOUNDS_SCREEN_MARGIN: i32 = 192;
const MODEL_BOUNDS_RUNTIME_RADIUS_PAD: i32 = 128;

#[derive(Clone, Copy, Default)]
struct ModelClipAnchor {
    floor_y: i32,
    pose_offset: [i32; 3],
    in_place: bool,
}

fn model_frame_bounds(
    runtime_model: RuntimeModelAsset,
    clip_local: ModelClipIndex,
    phase_q12: u32,
) -> Option<LevelModelFrameBoundsRecord> {
    let clip = runtime_model.clip_table_index(clip_local)?;
    let record = MODEL_CLIP_BOUNDS.get(clip.to_usize()).copied()?;
    if record.model != runtime_model.index || record.clip != clip || record.frame_count == 0 {
        return None;
    }
    let frame = ((phase_q12 >> 12) % record.frame_count as u32) as usize;
    MODEL_FRAME_BOUNDS
        .get(record.first_frame.to_usize().saturating_add(frame))
        .copied()
}

fn model_clip_anchor(
    runtime_model: RuntimeModelAsset,
    clip_local: ModelClipIndex,
) -> Option<ModelClipAnchor> {
    let clip = runtime_model.clip_table_index(clip_local)?;
    let record = MODEL_CLIP_BOUNDS.get(clip.to_usize()).copied()?;
    (record.model == runtime_model.index && record.clip == clip).then_some(ModelClipAnchor {
        floor_y: record.floor_y,
        pose_offset: record.pose_offset,
        in_place: (record.flags & model_clip_flags::IN_PLACE) != 0,
    })
}

fn model_bounds_visible(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    origin: WorldVertex,
    rotation: Mat3I16,
    bounds: LevelModelFrameBoundsRecord,
    visual_scale_q8: u16,
) -> bool {
    let center = rotate_bounds_center(
        rotation,
        scaled_bounds_center(bounds.center, visual_scale_q8),
    );
    let radius = scale_model_bounds_radius(bounds.radius, visual_scale_q8);
    sphere_visible_to_camera(
        camera,
        options,
        WorldVertex::new(
            origin.x.saturating_add(center[0]),
            origin.y.saturating_add(center[1]),
            origin.z.saturating_add(center[2]),
        ),
        radius
            .max(0)
            .saturating_add(MODEL_BOUNDS_RUNTIME_RADIUS_PAD),
        MODEL_BOUNDS_SCREEN_MARGIN,
    )
}

fn scaled_bounds_center(center: [i32; 3], visual_scale_q8: u16) -> [i32; 3] {
    [
        scale_q8_i32(center[0], visual_scale_q8),
        scale_q8_i32(center[1], visual_scale_q8),
        scale_q8_i32(center[2], visual_scale_q8),
    ]
}

fn scale_model_bounds_radius(radius: i32, visual_scale_q8: u16) -> i32 {
    scale_q8_i32(radius, visual_scale_q8)
}

fn scale_q8_i32(value: i32, scale_q8: u16) -> i32 {
    let scale = scale_q8.max(1) as i32;
    value.saturating_mul(scale) / MODEL_VISUAL_SCALE_ONE_Q8 as i32
}

fn rotate_bounds_center(rotation: Mat3I16, center: [i32; 3]) -> [i32; 3] {
    [
        dot_bounds_row_q12(rotation.m[0], center),
        dot_bounds_row_q12(rotation.m[1], center),
        dot_bounds_row_q12(rotation.m[2], center),
    ]
}

fn dot_bounds_row_q12(row: [i16; 3], center: [i32; 3]) -> i32 {
    let x = (row[0] as i32).saturating_mul(center[0]);
    let y = (row[1] as i32).saturating_mul(center[1]);
    let z = (row[2] as i32).saturating_mul(center[2]);
    x.saturating_add(y).saturating_add(z) >> 12
}

fn sphere_visible_to_camera(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    center: WorldVertex,
    radius: i32,
    screen_margin: i32,
) -> bool {
    let view = camera.view_vertex(center);
    let near = camera.projection.near_z.max(1);
    let far = options.depth_range.far().max(near);
    if view.z < near.saturating_sub(radius) || view.z > far.saturating_add(radius) {
        return false;
    }

    let z = view.z.max(near);
    let focal = camera.projection.focal_length.max(1);
    let half_w = (camera.projection.screen_x as i32)
        .saturating_add(screen_margin)
        .max(1);
    let half_h = (camera.projection.screen_y as i32)
        .saturating_add(screen_margin)
        .max(1);
    let projected_x = view.x.abs().saturating_sub(radius).saturating_mul(focal);
    let projected_y = view.y.abs().saturating_sub(radius).saturating_mul(focal);
    projected_x <= half_w.saturating_mul(z) && projected_y <= half_h.saturating_mul(z)
}

fn draw_image_props<T>(
    props: &[LevelImagePropRecord],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    let mut projector = None;
    for prop in props {
        if prop.room != current_room {
            continue;
        }
        let origin = WorldVertex::new(prop.x, prop.y, prop.z);
        let verts = image_prop_vertices(
            origin,
            prop.width,
            prop.height,
            prop.pitch,
            prop.yaw,
            prop.roll,
            prop.flags,
            *camera,
        );
        let (center, radius) = image_prop_cull_bounds(verts);
        if !sphere_visible_to_camera(camera, options, center, radius, 96) {
            continue;
        }
        let Some(asset) = find_asset_of_kind(ASSETS, prop.texture_asset, AssetKind::Texture) else {
            continue;
        };
        let Some(slot) = ensure_texture_uploaded_with_clut_mode(
            asset.id,
            asset.bytes,
            VramSlotClutMode::TransparentZero,
        ) else {
            continue;
        };
        let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, (0x80, 0x80, 0x80))
            .with_texture_window(slot.texture_window);
        let u_max = model_render_uv_max(slot.texture_width);
        let v_max = model_render_uv_max(slot.texture_height);
        let uvs = [(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)];
        if PROP_PARTICLE_GTE_PROJECT_ENABLED {
            let projector = match projector {
                Some(projector) => projector,
                None => {
                    let loaded = LoadedWorldCameraGte::load(*camera);
                    projector = Some(loaded);
                    loaded
                }
            };
            if let Some(projected) = projector.project_world_quad(verts) {
                let colors = [
                    lighting.apply_vertex_fog_weight(
                        prop.baked_vertex_rgb[0],
                        lighting.fog_weight_at_depth(projected[0].sz),
                    ),
                    lighting.apply_vertex_fog_weight(
                        prop.baked_vertex_rgb[1],
                        lighting.fog_weight_at_depth(projected[1].sz),
                    ),
                    lighting.apply_vertex_fog_weight(
                        prop.baked_vertex_rgb[2],
                        lighting.fog_weight_at_depth(projected[2].sz),
                    ),
                    lighting.apply_vertex_fog_weight(
                        prop.baked_vertex_rgb[3],
                        lighting.fog_weight_at_depth(projected[3].sz),
                    ),
                ];
                let sort_depth =
                    image_prop_sort_depth_projected(projected, camera.projection.near_z);
                let depth_bias = options
                    .depth_bias
                    .saturating_sub(image_prop_depth_bias(prop.width, prop.height));
                let opts = options
                    .with_depth_policy(DepthPolicy::Fixed(sort_depth))
                    .with_depth_bias(depth_bias)
                    .with_cull_mode(CullMode::None)
                    .with_material_layer(material)
                    .with_textured_triangle_splitting(true)
                    .with_textured_triangle_max_edge(0);
                let _ = world.submit_textured_gouraud_triangle(
                    triangles,
                    [projected[0], projected[1], projected[2]],
                    [uvs[0], uvs[1], uvs[2]],
                    [colors[0], colors[1], colors[2]],
                    material,
                    opts,
                );
                let _ = world.submit_textured_gouraud_triangle(
                    triangles,
                    [projected[0], projected[2], projected[3]],
                    [uvs[0], uvs[2], uvs[3]],
                    [colors[0], colors[2], colors[3]],
                    material,
                    opts,
                );
                continue;
            }
        }
        let colors = [
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[0], verts[0]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[1], verts[1]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[2], verts[2]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[3], verts[3]),
        ];
        if let Some(projected) = camera.project_world_quad(verts) {
            let sort_depth = image_prop_sort_depth_projected(projected, camera.projection.near_z);
            let depth_bias = options
                .depth_bias
                .saturating_sub(image_prop_depth_bias(prop.width, prop.height));
            let opts = options
                .with_depth_policy(DepthPolicy::Fixed(sort_depth))
                .with_depth_bias(depth_bias)
                .with_cull_mode(CullMode::None)
                .with_material_layer(material)
                .with_textured_triangle_splitting(true)
                .with_textured_triangle_max_edge(0);
            let _ = world.submit_textured_gouraud_triangle(
                triangles,
                [projected[0], projected[1], projected[2]],
                [uvs[0], uvs[1], uvs[2]],
                [colors[0], colors[1], colors[2]],
                material,
                opts,
            );
            let _ = world.submit_textured_gouraud_triangle(
                triangles,
                [projected[0], projected[2], projected[3]],
                [uvs[0], uvs[2], uvs[3]],
                [colors[0], colors[2], colors[3]],
                material,
                opts,
            );
        } else {
            let tint = average_vertex_rgb(colors);
            let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, tint)
                .with_texture_window(slot.texture_window);
            let sort_depth = image_prop_sort_depth(camera, verts);
            let depth_bias = options
                .depth_bias
                .saturating_sub(image_prop_depth_bias(prop.width, prop.height));
            let opts = options
                .with_depth_policy(DepthPolicy::Fixed(sort_depth))
                .with_depth_bias(depth_bias)
                .with_cull_mode(CullMode::None)
                .with_material_layer(material)
                .with_textured_triangle_splitting(true)
                .with_textured_triangle_max_edge(0);
            let _ =
                world.submit_textured_world_quad(triangles, *camera, verts, uvs, material, opts);
        }
    }
}

const BOX_PROP_FACE_VERTEX_INDICES: [[usize; 4]; psx_level::BOX_PROP_FACE_COUNT] = [
    [4, 5, 1, 0],
    [5, 6, 2, 1],
    [6, 7, 3, 2],
    [7, 4, 0, 3],
    [7, 6, 5, 4],
    [0, 1, 2, 3],
];

const BOX_PROP_BREAK_SHARDS: [BoxPropBreakShard; 20] = [
    BoxPropBreakShard {
        face: 0,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 84,
        v1_q8: 256,
        drift_q8_per_frame: -3,
        lift_per_frame: 28,
        impulse_per_frame: 34,
        twist_q8_per_frame: -4,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 0,
        u0_q8: 84,
        v0_q8: 0,
        u1_q8: 172,
        v1_q8: 256,
        drift_q8_per_frame: 1,
        lift_per_frame: 36,
        impulse_per_frame: 40,
        twist_q8_per_frame: 5,
        delay: 1,
    },
    BoxPropBreakShard {
        face: 0,
        u0_q8: 172,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        drift_q8_per_frame: 4,
        lift_per_frame: 24,
        impulse_per_frame: 31,
        twist_q8_per_frame: -6,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 1,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 86,
        v1_q8: 256,
        drift_q8_per_frame: -4,
        lift_per_frame: 30,
        impulse_per_frame: 36,
        twist_q8_per_frame: 6,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 1,
        u0_q8: 86,
        v0_q8: 0,
        u1_q8: 170,
        v1_q8: 256,
        drift_q8_per_frame: 2,
        lift_per_frame: 38,
        impulse_per_frame: 42,
        twist_q8_per_frame: -4,
        delay: 1,
    },
    BoxPropBreakShard {
        face: 1,
        u0_q8: 170,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        drift_q8_per_frame: 5,
        lift_per_frame: 26,
        impulse_per_frame: 32,
        twist_q8_per_frame: 5,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 2,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 84,
        v1_q8: 256,
        drift_q8_per_frame: -5,
        lift_per_frame: 24,
        impulse_per_frame: 30,
        twist_q8_per_frame: 5,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 2,
        u0_q8: 84,
        v0_q8: 0,
        u1_q8: 172,
        v1_q8: 256,
        drift_q8_per_frame: -1,
        lift_per_frame: 34,
        impulse_per_frame: 38,
        twist_q8_per_frame: -5,
        delay: 2,
    },
    BoxPropBreakShard {
        face: 2,
        u0_q8: 172,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        drift_q8_per_frame: 3,
        lift_per_frame: 28,
        impulse_per_frame: 35,
        twist_q8_per_frame: 7,
        delay: 1,
    },
    BoxPropBreakShard {
        face: 3,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 86,
        v1_q8: 256,
        drift_q8_per_frame: -4,
        lift_per_frame: 32,
        impulse_per_frame: 34,
        twist_q8_per_frame: -6,
        delay: 1,
    },
    BoxPropBreakShard {
        face: 3,
        u0_q8: 86,
        v0_q8: 0,
        u1_q8: 170,
        v1_q8: 256,
        drift_q8_per_frame: 1,
        lift_per_frame: 40,
        impulse_per_frame: 41,
        twist_q8_per_frame: 4,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 3,
        u0_q8: 170,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        drift_q8_per_frame: 4,
        lift_per_frame: 25,
        impulse_per_frame: 33,
        twist_q8_per_frame: -5,
        delay: 2,
    },
    BoxPropBreakShard {
        face: 4,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 128,
        v1_q8: 128,
        drift_q8_per_frame: -3,
        lift_per_frame: 48,
        impulse_per_frame: 26,
        twist_q8_per_frame: 5,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 4,
        u0_q8: 128,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 128,
        drift_q8_per_frame: 4,
        lift_per_frame: 44,
        impulse_per_frame: 28,
        twist_q8_per_frame: -5,
        delay: 1,
    },
    BoxPropBreakShard {
        face: 4,
        u0_q8: 0,
        v0_q8: 128,
        u1_q8: 128,
        v1_q8: 256,
        drift_q8_per_frame: -5,
        lift_per_frame: 42,
        impulse_per_frame: 24,
        twist_q8_per_frame: -4,
        delay: 2,
    },
    BoxPropBreakShard {
        face: 4,
        u0_q8: 128,
        v0_q8: 128,
        u1_q8: 256,
        v1_q8: 256,
        drift_q8_per_frame: 3,
        lift_per_frame: 50,
        impulse_per_frame: 30,
        twist_q8_per_frame: 6,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 5,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 128,
        v1_q8: 128,
        drift_q8_per_frame: -2,
        lift_per_frame: 16,
        impulse_per_frame: 24,
        twist_q8_per_frame: -4,
        delay: 3,
    },
    BoxPropBreakShard {
        face: 5,
        u0_q8: 128,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 128,
        drift_q8_per_frame: 3,
        lift_per_frame: 14,
        impulse_per_frame: 22,
        twist_q8_per_frame: 4,
        delay: 4,
    },
    BoxPropBreakShard {
        face: 5,
        u0_q8: 0,
        v0_q8: 128,
        u1_q8: 128,
        v1_q8: 256,
        drift_q8_per_frame: -4,
        lift_per_frame: 12,
        impulse_per_frame: 20,
        twist_q8_per_frame: 3,
        delay: 4,
    },
    BoxPropBreakShard {
        face: 5,
        u0_q8: 128,
        v0_q8: 128,
        u1_q8: 256,
        v1_q8: 256,
        drift_q8_per_frame: 4,
        lift_per_frame: 18,
        impulse_per_frame: 25,
        twist_q8_per_frame: -3,
        delay: 3,
    },
];

const BOX_PROP_FLOOR_DEBRIS_CHIPS: [BoxPropFloorDebrisChip; 12] = [
    BoxPropFloorDebrisChip {
        face: 0,
        offset_x_q8: -80,
        offset_z_q8: -72,
        half_length_q8: 46,
        half_width_q8: 13,
        yaw_q12: 384,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 84,
        v1_q8: 256,
        lift: 6,
    },
    BoxPropFloorDebrisChip {
        face: 0,
        offset_x_q8: 38,
        offset_z_q8: -94,
        half_length_q8: 58,
        half_width_q8: 12,
        yaw_q12: 960,
        u0_q8: 84,
        v0_q8: 0,
        u1_q8: 172,
        v1_q8: 256,
        lift: 8,
    },
    BoxPropFloorDebrisChip {
        face: 1,
        offset_x_q8: 104,
        offset_z_q8: -24,
        half_length_q8: 42,
        half_width_q8: 15,
        yaw_q12: 1328,
        u0_q8: 170,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        lift: 7,
    },
    BoxPropFloorDebrisChip {
        face: 1,
        offset_x_q8: 42,
        offset_z_q8: 72,
        half_length_q8: 54,
        half_width_q8: 13,
        yaw_q12: 1888,
        u0_q8: 0,
        v0_q8: 16,
        u1_q8: 86,
        v1_q8: 240,
        lift: 10,
    },
    BoxPropFloorDebrisChip {
        face: 2,
        offset_x_q8: -96,
        offset_z_q8: 44,
        half_length_q8: 50,
        half_width_q8: 11,
        yaw_q12: 2384,
        u0_q8: 84,
        v0_q8: 16,
        u1_q8: 172,
        v1_q8: 240,
        lift: 9,
    },
    BoxPropFloorDebrisChip {
        face: 2,
        offset_x_q8: -28,
        offset_z_q8: 104,
        half_length_q8: 34,
        half_width_q8: 16,
        yaw_q12: 3040,
        u0_q8: 172,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        lift: 6,
    },
    BoxPropFloorDebrisChip {
        face: 3,
        offset_x_q8: -132,
        offset_z_q8: -10,
        half_length_q8: 44,
        half_width_q8: 13,
        yaw_q12: 3536,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 86,
        v1_q8: 256,
        lift: 8,
    },
    BoxPropFloorDebrisChip {
        face: 3,
        offset_x_q8: 116,
        offset_z_q8: 92,
        half_length_q8: 32,
        half_width_q8: 14,
        yaw_q12: 256,
        u0_q8: 86,
        v0_q8: 24,
        u1_q8: 170,
        v1_q8: 232,
        lift: 11,
    },
    BoxPropFloorDebrisChip {
        face: 4,
        offset_x_q8: -24,
        offset_z_q8: -8,
        half_length_q8: 62,
        half_width_q8: 24,
        yaw_q12: 704,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 128,
        v1_q8: 128,
        lift: 5,
    },
    BoxPropFloorDebrisChip {
        face: 4,
        offset_x_q8: 82,
        offset_z_q8: 36,
        half_length_q8: 40,
        half_width_q8: 20,
        yaw_q12: 2656,
        u0_q8: 128,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 128,
        lift: 7,
    },
    BoxPropFloorDebrisChip {
        face: 5,
        offset_x_q8: -54,
        offset_z_q8: 84,
        half_length_q8: 36,
        half_width_q8: 18,
        yaw_q12: 1536,
        u0_q8: 0,
        v0_q8: 128,
        u1_q8: 128,
        v1_q8: 256,
        lift: 6,
    },
    BoxPropFloorDebrisChip {
        face: 5,
        offset_x_q8: 8,
        offset_z_q8: -126,
        half_length_q8: 30,
        half_width_q8: 16,
        yaw_q12: 3264,
        u0_q8: 128,
        v0_q8: 128,
        u1_q8: 256,
        v1_q8: 256,
        lift: 9,
    },
];

fn box_prop_state_bit(index: usize) -> Option<(usize, u32)> {
    if index >= MAX_BOX_PROP_STATE {
        return None;
    }
    Some((index / 32, 1u32 << (index % 32)))
}

fn box_prop_broken_in_words(broken: &[u32; BOX_PROP_BROKEN_WORDS], index: usize) -> bool {
    let Some((word, mask)) = box_prop_state_bit(index) else {
        return false;
    };
    broken[word] & mask != 0
}

fn draw_box_props<T>(
    props: &[LevelBoxPropRecord],
    broken: &[u32; BOX_PROP_BROKEN_WORDS],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    for (index, prop) in props.iter().enumerate() {
        if prop.room != current_room || box_prop_broken_in_words(broken, index) {
            continue;
        }
        let vertices = box_prop_vertices(prop);
        let (center, radius) = box_prop_cull_bounds(vertices);
        if !sphere_visible_to_camera(camera, options, center, radius, 96) {
            continue;
        }
        draw_box_prop_faces(
            prop,
            box_prop_faces(vertices),
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

fn draw_box_prop_floor_debris<T>(
    props: &[LevelBoxPropRecord],
    broken: &[u32; BOX_PROP_BROKEN_WORDS],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    for (index, prop) in props.iter().enumerate() {
        if prop.room != current_room || !box_prop_broken_in_words(broken, index) {
            continue;
        }
        let vertices = box_prop_vertices(prop);
        let (center, radius) = box_prop_cull_bounds(vertices);
        let floor_y = box_prop_floor_y(vertices);
        let debris_center = WorldVertex::new(center.x, floor_y.saturating_add(16), center.z);
        if !sphere_visible_to_camera(
            camera,
            options,
            debris_center,
            radius.saturating_mul(2),
            128,
        ) {
            continue;
        }
        draw_box_prop_floor_debris_chips(
            prop, vertices, floor_y, camera, options, lighting, triangles, world,
        );
    }
}

fn draw_box_prop_floor_debris_chips<T>(
    prop: &LevelBoxPropRecord,
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
    floor_y: i32,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    let bounds = box_prop_debris_bounds(vertices);
    for chip in BOX_PROP_FLOOR_DEBRIS_CHIPS {
        let face = chip.face as usize;
        if face >= psx_level::BOX_PROP_FACE_COUNT {
            continue;
        }
        draw_box_prop_floor_debris_chip(
            prop, face, bounds, floor_y, chip, camera, options, lighting, triangles, world,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_prop_floor_debris_chip<T>(
    prop: &LevelBoxPropRecord,
    face: usize,
    bounds: BoxPropDebrisBounds,
    floor_y: i32,
    chip: BoxPropFloorDebrisChip,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    let Some(texture_asset) = prop.texture_assets[face] else {
        return;
    };
    let Some(asset) = find_asset_of_kind(ASSETS, texture_asset, AssetKind::Texture) else {
        return;
    };
    let Some(slot) = ensure_texture_uploaded_with_clut_mode(
        asset.id,
        asset.bytes,
        VramSlotClutMode::TransparentZero,
    ) else {
        return;
    };

    let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, (0x80, 0x80, 0x80))
        .with_texture_window(slot.texture_window);
    let u_max = model_render_uv_max(slot.texture_width);
    let v_max = model_render_uv_max(slot.texture_height);
    let uvs = box_prop_floor_debris_uvs(u_max, v_max, chip);
    let quad = box_prop_floor_debris_quad(bounds, floor_y, chip);
    let colors = [
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, chip.u0_q8, chip.v0_q8),
            quad[0],
        ),
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, chip.u1_q8, chip.v0_q8),
            quad[1],
        ),
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, chip.u1_q8, chip.v1_q8),
            quad[2],
        ),
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, chip.u0_q8, chip.v1_q8),
            quad[3],
        ),
    ];
    let opts = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::None)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(0);
    if let Some(projected) = camera.project_world_quad(quad) {
        let _ = world.submit_textured_gouraud_triangle(
            triangles,
            [projected[0], projected[1], projected[2]],
            [uvs[0], uvs[1], uvs[2]],
            [colors[0], colors[1], colors[2]],
            material,
            opts,
        );
        let _ = world.submit_textured_gouraud_triangle(
            triangles,
            [projected[0], projected[2], projected[3]],
            [uvs[0], uvs[2], uvs[3]],
            [colors[0], colors[2], colors[3]],
            material,
            opts,
        );
    } else {
        let tint = average_vertex_rgb(colors);
        let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, tint)
            .with_texture_window(slot.texture_window);
        let opts = opts.with_material_layer(material);
        let _ = world.submit_textured_world_quad(triangles, *camera, quad, uvs, material, opts);
    }
}

fn draw_box_prop_break_events<T>(
    events: &[BoxPropBreakEvent; MAX_BOX_PROP_BREAK_EVENTS],
    props: &[LevelBoxPropRecord],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    for event in events {
        if !event.is_active() || event.age >= BOX_PROP_BREAK_FRAMES {
            continue;
        }
        let Some(prop) = props.get(event.prop_index as usize) else {
            continue;
        };
        if prop.room != current_room {
            continue;
        }
        let vertices = box_prop_vertices(prop);
        let (center, radius) = box_prop_cull_bounds(vertices);
        if !sphere_visible_to_camera(camera, options, center, radius.saturating_mul(3), 128) {
            continue;
        }
        draw_box_prop_break_shards(
            prop,
            box_prop_faces(vertices),
            center,
            *event,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

fn draw_box_prop_break_shards<T>(
    prop: &LevelBoxPropRecord,
    faces: [[WorldVertex; 4]; psx_level::BOX_PROP_FACE_COUNT],
    box_center: WorldVertex,
    event: BoxPropBreakEvent,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    for (shard_index, shard) in BOX_PROP_BREAK_SHARDS.iter().copied().enumerate() {
        if event.age < shard.delay {
            continue;
        }
        let face = shard.face as usize;
        if face >= psx_level::BOX_PROP_FACE_COUNT {
            continue;
        }
        draw_box_prop_break_shard(
            prop,
            face,
            faces[face],
            box_center,
            event,
            shard,
            shard_index,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_prop_break_shard<T>(
    prop: &LevelBoxPropRecord,
    face: usize,
    face_vertices: [WorldVertex; 4],
    box_center: WorldVertex,
    event: BoxPropBreakEvent,
    shard: BoxPropBreakShard,
    shard_index: usize,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    let Some(texture_asset) = prop.texture_assets[face] else {
        return;
    };
    let Some(asset) = find_asset_of_kind(ASSETS, texture_asset, AssetKind::Texture) else {
        return;
    };
    let Some(slot) = ensure_texture_uploaded_with_clut_mode(
        asset.id,
        asset.bytes,
        VramSlotClutMode::TransparentZero,
    ) else {
        return;
    };

    let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, (0x80, 0x80, 0x80))
        .with_texture_window(slot.texture_window);
    let u_max = model_render_uv_max(slot.texture_width);
    let v_max = model_render_uv_max(slot.texture_height);
    let uvs = box_prop_shard_uvs(u_max, v_max, shard);
    let quad = box_prop_break_shard_quad(face_vertices, box_center, event, shard, shard_index);
    let colors = [
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, shard.u0_q8, shard.v0_q8),
            quad[0],
        ),
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, shard.u1_q8, shard.v0_q8),
            quad[1],
        ),
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, shard.u1_q8, shard.v1_q8),
            quad[2],
        ),
        lighting.apply_vertex_fog(
            box_prop_face_color_at(prop, face, shard.u0_q8, shard.v1_q8),
            quad[3],
        ),
    ];
    let opts = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::None)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(0);
    if let Some(projected) = camera.project_world_quad(quad) {
        let _ = world.submit_textured_gouraud_triangle(
            triangles,
            [projected[0], projected[1], projected[2]],
            [uvs[0], uvs[1], uvs[2]],
            [colors[0], colors[1], colors[2]],
            material,
            opts,
        );
        let _ = world.submit_textured_gouraud_triangle(
            triangles,
            [projected[0], projected[2], projected[3]],
            [uvs[0], uvs[2], uvs[3]],
            [colors[0], colors[2], colors[3]],
            material,
            opts,
        );
    } else {
        let tint = average_vertex_rgb(colors);
        let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, tint)
            .with_texture_window(slot.texture_window);
        let opts = opts.with_material_layer(material);
        let _ = world.submit_textured_world_quad(triangles, *camera, quad, uvs, material, opts);
    }
}

fn draw_box_prop_faces<T>(
    prop: &LevelBoxPropRecord,
    faces: [[WorldVertex; 4]; psx_level::BOX_PROP_FACE_COUNT],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    for face in 0..psx_level::BOX_PROP_FACE_COUNT {
        let face_vertices = faces[face];
        if !box_prop_face_front_facing(camera, face_vertices) {
            continue;
        }
        let Some(texture_asset) = prop.texture_assets[face] else {
            continue;
        };
        // Box props share the texture pool with room surfaces, so the texture
        // is already resident: look the slot up directly and skip the per-face
        // linear ASSETS scan + upload check. Fall back to the full
        // resolve-and-upload path only on a (rare) cold miss. `find_vram_slot`
        // returns exactly the slot `ensure_texture_uploaded` would for a
        // resident asset, so this is bit-identical.
        let slot = match find_vram_slot(texture_asset, VramSlotClutMode::TransparentZero) {
            Some(slot) => slot,
            None => {
                let Some(asset) = find_asset_of_kind(ASSETS, texture_asset, AssetKind::Texture)
                else {
                    continue;
                };
                match ensure_texture_uploaded_with_clut_mode(
                    asset.id,
                    asset.bytes,
                    VramSlotClutMode::TransparentZero,
                ) {
                    Some(slot) => slot,
                    None => continue,
                }
            }
        };
        let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, (0x80, 0x80, 0x80))
            .with_texture_window(slot.texture_window);
        let u_max = model_render_uv_max(slot.texture_width);
        let v_max = model_render_uv_max(slot.texture_height);
        let uvs = [(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)];
        let colors = [
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][0], face_vertices[0]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][1], face_vertices[1]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][2], face_vertices[2]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][3], face_vertices[3]),
        ];
        let opts = options
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(CullMode::None)
            .with_material_layer(material)
            .with_textured_triangle_splitting(true)
            .with_textured_triangle_max_edge(0);
        if let Some(projected) = camera.project_world_quad(face_vertices) {
            let _ = world.submit_textured_gouraud_triangle(
                triangles,
                [projected[0], projected[1], projected[2]],
                [uvs[0], uvs[1], uvs[2]],
                [colors[0], colors[1], colors[2]],
                material,
                opts,
            );
            let _ = world.submit_textured_gouraud_triangle(
                triangles,
                [projected[0], projected[2], projected[3]],
                [uvs[0], uvs[2], uvs[3]],
                [colors[0], colors[2], colors[3]],
                material,
                opts,
            );
        } else {
            let tint = average_vertex_rgb(colors);
            let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, tint)
                .with_texture_window(slot.texture_window);
            let opts = opts.with_material_layer(material);
            let _ = world.submit_textured_world_quad(
                triangles,
                *camera,
                face_vertices,
                uvs,
                material,
                opts,
            );
        }
    }
}

fn box_prop_face_front_facing(camera: &WorldCamera, face: [WorldVertex; 4]) -> bool {
    let abx = face[1].x.saturating_sub(face[0].x);
    let aby = face[1].y.saturating_sub(face[0].y);
    let abz = face[1].z.saturating_sub(face[0].z);
    let acx = face[2].x.saturating_sub(face[0].x);
    let acy = face[2].y.saturating_sub(face[0].y);
    let acz = face[2].z.saturating_sub(face[0].z);
    let nx = aby
        .saturating_mul(acz)
        .saturating_sub(abz.saturating_mul(acy))
        >> BOX_PROP_FACE_NORMAL_SHIFT;
    let ny = abz
        .saturating_mul(acx)
        .saturating_sub(abx.saturating_mul(acz))
        >> BOX_PROP_FACE_NORMAL_SHIFT;
    let nz = abx
        .saturating_mul(acy)
        .saturating_sub(aby.saturating_mul(acx))
        >> BOX_PROP_FACE_NORMAL_SHIFT;
    let center = WorldVertex::new(
        average4_i32(face[0].x, face[1].x, face[2].x, face[3].x),
        average4_i32(face[0].y, face[1].y, face[2].y, face[3].y),
        average4_i32(face[0].z, face[1].z, face[2].z, face[3].z),
    );
    let vx = camera.position.x.saturating_sub(center.x);
    let vy = camera.position.y.saturating_sub(center.y);
    let vz = camera.position.z.saturating_sub(center.z);
    nx.saturating_mul(vx)
        .saturating_add(ny.saturating_mul(vy))
        .saturating_add(nz.saturating_mul(vz))
        > 0
}

fn box_prop_break_shard_quad(
    face: [WorldVertex; 4],
    box_center: WorldVertex,
    event: BoxPropBreakEvent,
    shard: BoxPropBreakShard,
    shard_index: usize,
) -> [WorldVertex; 4] {
    let age = event
        .age
        .saturating_sub(shard.delay)
        .min(BOX_PROP_BREAK_MOTION_FRAMES) as i32;
    let mut quad = [
        box_prop_face_point_q8(face, shard.u0_q8, shard.v0_q8),
        box_prop_face_point_q8(face, shard.u1_q8, shard.v0_q8),
        box_prop_face_point_q8(face, shard.u1_q8, shard.v1_q8),
        box_prop_face_point_q8(face, shard.u0_q8, shard.v1_q8),
    ];
    let shard_center = box_prop_quad_center(quad);
    let face_center = box_prop_quad_center(face);
    let edge_u = world_vertex_delta(face[0], face[1]);
    let edge_v = world_vertex_delta(face[0], face[3]);
    let spin_q12 = box_prop_break_shard_spin_q12(event.prop_index, shard_index, age);
    let outward_q8 = age.saturating_mul(age);
    let drift_q8 = (shard.drift_q8_per_frame as i32)
        .saturating_mul(age)
        .clamp(-96, 96);
    let twist_q8 = (shard.twist_q8_per_frame as i32)
        .saturating_mul(age)
        .clamp(-96, 96);
    let shrink_q8 = (252 - age.saturating_mul(3)).max(176);
    let impulse_units = age.saturating_mul(shard.impulse_per_frame as i32);
    let fall = age.saturating_mul(age).saturating_mul(4);
    let face_delta = world_vertex_delta(box_center, face_center);
    let drift = scale_world_delta_q8(edge_u, drift_q8);
    let offset = [
        scale_q8_i32_signed(face_delta[0], outward_q8)
            .saturating_add((event.impulse_x_q8 as i32).saturating_mul(impulse_units) / 256)
            .saturating_add(drift[0]),
        scale_q8_i32_signed(face_delta[1], outward_q8)
            .saturating_add((shard.lift_per_frame as i32).saturating_mul(age))
            .saturating_sub(fall)
            .saturating_add(drift[1]),
        scale_q8_i32_signed(face_delta[2], outward_q8)
            .saturating_add((event.impulse_z_q8 as i32).saturating_mul(impulse_units) / 256)
            .saturating_add(drift[2]),
    ];

    for (corner, vertex) in quad.iter_mut().enumerate() {
        let mut p = shrink_world_vertex_around(*vertex, shard_center, shrink_q8);
        let sign_u = if corner == 0 || corner == 3 { -1 } else { 1 };
        let sign_v = if corner == 0 || corner == 1 { -1 } else { 1 };
        let tumble_u = scale_world_delta_q8(edge_u, sign_v * twist_q8 / 2);
        let tumble_v = scale_world_delta_q8(edge_v, -sign_u * twist_q8);
        p = add_world_vertex_offset(p, tumble_u);
        p = add_world_vertex_offset(p, tumble_v);
        p = rotate_world_vertex_y_around_q12(p, shard_center, spin_q12);
        *vertex = add_world_vertex_offset(p, offset);
    }
    quad
}

fn box_prop_break_shard_spin_q12(prop_index: u16, shard_index: usize, age: i32) -> u16 {
    let seed = (prop_index as u32)
        .wrapping_mul(73)
        .wrapping_add((shard_index as u32).wrapping_mul(151))
        .wrapping_add(0x4d3);
    let speed = 4 + (seed & 0x0f) as i32;
    let wobble = (((seed >> 5) & 0x07) as i32).saturating_sub(3);
    let signed = age.saturating_mul(speed.saturating_add(wobble).max(2));
    let spin = if seed & 0x10 == 0 { signed } else { -signed };
    spin.rem_euclid(4096) as u16
}

fn rotate_world_vertex_y_around_q12(
    vertex: WorldVertex,
    center: WorldVertex,
    angle_q12: u16,
) -> WorldVertex {
    if angle_q12 == 0 {
        return vertex;
    }
    let relative = [
        vertex.x.saturating_sub(center.x),
        vertex.y.saturating_sub(center.y),
        vertex.z.saturating_sub(center.z),
    ];
    let rotated = rotate_y_q12(relative, angle_q12);
    WorldVertex::new(
        center.x.saturating_add(rotated[0]),
        center.y.saturating_add(rotated[1]),
        center.z.saturating_add(rotated[2]),
    )
}

fn box_prop_floor_debris_quad(
    bounds: BoxPropDebrisBounds,
    floor_y: i32,
    chip: BoxPropFloorDebrisChip,
) -> [WorldVertex; 4] {
    let base = bounds.span_x.max(bounds.span_z).max(128);
    let half_length = (base.saturating_mul(chip.half_length_q8 as i32) / 256).clamp(32, base);
    let half_width = (base.saturating_mul(chip.half_width_q8 as i32) / 256).clamp(16, base);
    let center_x = bounds
        .center_x
        .saturating_add(bounds.span_x.saturating_mul(chip.offset_x_q8 as i32) / 256);
    let center_z = bounds
        .center_z
        .saturating_add(bounds.span_z.saturating_mul(chip.offset_z_q8 as i32) / 256);
    let long = rotate_y_q12([half_length, 0, 0], chip.yaw_q12);
    let short = rotate_y_q12([0, 0, half_width], chip.yaw_q12);
    let y = floor_y.saturating_add(chip.lift as i32);
    [
        WorldVertex::new(
            center_x - long[0] - short[0],
            y,
            center_z - long[2] - short[2],
        ),
        WorldVertex::new(
            center_x + long[0] - short[0],
            y,
            center_z + long[2] - short[2],
        ),
        WorldVertex::new(
            center_x + long[0] + short[0],
            y,
            center_z + long[2] + short[2],
        ),
        WorldVertex::new(
            center_x - long[0] + short[0],
            y,
            center_z - long[2] + short[2],
        ),
    ]
}

fn box_prop_floor_debris_uvs(u_max: u8, v_max: u8, chip: BoxPropFloorDebrisChip) -> [(u8, u8); 4] {
    let u0 = uv_from_q8(u_max, chip.u0_q8);
    let u1 = uv_from_q8(u_max, chip.u1_q8);
    let v0 = uv_from_q8(v_max, chip.v0_q8);
    let v1 = uv_from_q8(v_max, chip.v1_q8);
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
}

fn box_prop_shard_uvs(u_max: u8, v_max: u8, shard: BoxPropBreakShard) -> [(u8, u8); 4] {
    let u0 = uv_from_q8(u_max, shard.u0_q8);
    let u1 = uv_from_q8(u_max, shard.u1_q8);
    let v0 = uv_from_q8(v_max, shard.v0_q8);
    let v1 = uv_from_q8(v_max, shard.v1_q8);
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
}

fn box_prop_face_point_q8(face: [WorldVertex; 4], u_q8: u16, v_q8: u16) -> WorldVertex {
    let left = lerp_world_vertex_q8(face[0], face[3], v_q8);
    let right = lerp_world_vertex_q8(face[1], face[2], v_q8);
    lerp_world_vertex_q8(left, right, u_q8)
}

fn box_prop_quad_center(quad: [WorldVertex; 4]) -> WorldVertex {
    WorldVertex::new(
        average4_i32(quad[0].x, quad[1].x, quad[2].x, quad[3].x),
        average4_i32(quad[0].y, quad[1].y, quad[2].y, quad[3].y),
        average4_i32(quad[0].z, quad[1].z, quad[2].z, quad[3].z),
    )
}

fn box_prop_face_color_at(
    prop: &LevelBoxPropRecord,
    face: usize,
    u_q8: u16,
    v_q8: u16,
) -> (u8, u8, u8) {
    let colors = prop.baked_vertex_rgb[face];
    let top = lerp_rgb_q8(colors[0], colors[1], u_q8);
    let bottom = lerp_rgb_q8(colors[3], colors[2], u_q8);
    lerp_rgb_q8(top, bottom, v_q8)
}

fn lerp_world_vertex_q8(a: WorldVertex, b: WorldVertex, t_q8: u16) -> WorldVertex {
    WorldVertex::new(
        lerp_i32_q8(a.x, b.x, t_q8),
        lerp_i32_q8(a.y, b.y, t_q8),
        lerp_i32_q8(a.z, b.z, t_q8),
    )
}

fn lerp_rgb_q8(a: (u8, u8, u8), b: (u8, u8, u8), t_q8: u16) -> (u8, u8, u8) {
    (
        lerp_i32_q8(a.0 as i32, b.0 as i32, t_q8) as u8,
        lerp_i32_q8(a.1 as i32, b.1 as i32, t_q8) as u8,
        lerp_i32_q8(a.2 as i32, b.2 as i32, t_q8) as u8,
    )
}

fn lerp_i32_q8(a: i32, b: i32, t_q8: u16) -> i32 {
    let t = t_q8.min(256) as i32;
    a.saturating_add(b.saturating_sub(a).saturating_mul(t) / 256)
}

fn uv_from_q8(max: u8, t_q8: u16) -> u8 {
    ((max as u16).saturating_mul(t_q8.min(256)) / 256) as u8
}

fn shrink_world_vertex_around(
    vertex: WorldVertex,
    center: WorldVertex,
    scale_q8: i32,
) -> WorldVertex {
    WorldVertex::new(
        center.x.saturating_add(scale_q8_i32_signed(
            vertex.x.saturating_sub(center.x),
            scale_q8,
        )),
        center.y.saturating_add(scale_q8_i32_signed(
            vertex.y.saturating_sub(center.y),
            scale_q8,
        )),
        center.z.saturating_add(scale_q8_i32_signed(
            vertex.z.saturating_sub(center.z),
            scale_q8,
        )),
    )
}

fn world_vertex_delta(from: WorldVertex, to: WorldVertex) -> [i32; 3] {
    [
        to.x.saturating_sub(from.x),
        to.y.saturating_sub(from.y),
        to.z.saturating_sub(from.z),
    ]
}

fn scale_world_delta_q8(delta: [i32; 3], scale_q8: i32) -> [i32; 3] {
    [
        scale_q8_i32_signed(delta[0], scale_q8),
        scale_q8_i32_signed(delta[1], scale_q8),
        scale_q8_i32_signed(delta[2], scale_q8),
    ]
}

fn add_world_vertex_offset(vertex: WorldVertex, offset: [i32; 3]) -> WorldVertex {
    WorldVertex::new(
        vertex.x.saturating_add(offset[0]),
        vertex.y.saturating_add(offset[1]),
        vertex.z.saturating_add(offset[2]),
    )
}

fn scale_q8_i32_signed(value: i32, scale_q8: i32) -> i32 {
    value.saturating_mul(scale_q8) / 256
}

fn average_vertex_rgb(colors: [(u8, u8, u8); 4]) -> (u8, u8, u8) {
    let mut r = 0u16;
    let mut g = 0u16;
    let mut b = 0u16;
    for color in colors {
        r += color.0 as u16;
        g += color.1 as u16;
        b += color.2 as u16;
    }
    ((r / 4) as u8, (g / 4) as u8, (b / 4) as u8)
}

fn image_prop_depth_bias(width: u16, height: u16) -> i32 {
    IMAGE_PROP_DEPTH_BIAS.saturating_add((width.max(height) as i32) / 2)
}

fn image_prop_cull_bounds(verts: [WorldVertex; 4]) -> (WorldVertex, i32) {
    let center = WorldVertex::new(
        average4_i32(verts[0].x, verts[1].x, verts[2].x, verts[3].x),
        average4_i32(verts[0].y, verts[1].y, verts[2].y, verts[3].y),
        average4_i32(verts[0].z, verts[1].z, verts[2].z, verts[3].z),
    );
    let mut radius = 32;
    for vertex in verts {
        let dx = abs_delta_i32(vertex.x, center.x);
        let dy = abs_delta_i32(vertex.y, center.y);
        let dz = abs_delta_i32(vertex.z, center.z);
        radius = radius.max(dx.saturating_add(dy).saturating_add(dz));
    }
    (center, radius)
}

fn average4_i32(a: i32, b: i32, c: i32, d: i32) -> i32 {
    a.saturating_add(b).saturating_add(c).saturating_add(d) / 4
}

fn abs_delta_i32(a: i32, b: i32) -> i32 {
    if a >= b {
        a.saturating_sub(b)
    } else {
        b.saturating_sub(a)
    }
}

fn image_prop_sort_depth(camera: &WorldCamera, verts: [WorldVertex; 4]) -> i32 {
    let mut nearest = i32::MAX;
    for vertex in verts {
        nearest = nearest.min(camera.view_vertex(vertex).z);
    }
    nearest.max(camera.projection.near_z)
}

fn image_prop_sort_depth_projected(verts: [ProjectedVertex; 4], near_z: i32) -> i32 {
    let mut nearest = i32::MAX;
    for vertex in verts {
        nearest = nearest.min(vertex.sz);
    }
    nearest.max(near_z)
}

fn image_prop_vertices(
    origin: WorldVertex,
    width: u16,
    height: u16,
    pitch: i16,
    yaw: i16,
    roll: i16,
    flags: u16,
    camera: WorldCamera,
) -> [WorldVertex; 4] {
    if flags & image_prop_flags::CYLINDRICAL_BILLBOARD != 0 {
        let half_width = (width as i32) / 2;
        let right_x = mul_q12_i32(half_width, camera.cos_yaw.raw());
        let right_z = -mul_q12_i32(half_width, camera.sin_yaw.raw());
        let top_y = origin.y.saturating_add(height as i32);
        return [
            WorldVertex::new(origin.x - right_x, top_y, origin.z - right_z),
            WorldVertex::new(origin.x + right_x, top_y, origin.z + right_z),
            WorldVertex::new(origin.x + right_x, origin.y, origin.z + right_z),
            WorldVertex::new(origin.x - right_x, origin.y, origin.z - right_z),
        ];
    }

    let half_width = (width as i32) / 2;
    let h = height as i32;
    let locals = [
        [-half_width, h, 0],
        [half_width, h, 0],
        [half_width, 0, 0],
        [-half_width, 0, 0],
    ];
    let mut out = [WorldVertex::new(0, 0, 0); 4];
    let mut i = 0usize;
    while i < locals.len() {
        let rotated = rotate_z_q12(
            rotate_y_q12(rotate_x_q12(locals[i], pitch as u16), yaw as u16),
            roll as u16,
        );
        out[i] = WorldVertex::new(
            origin.x.saturating_add(rotated[0]),
            origin.y.saturating_add(rotated[1]),
            origin.z.saturating_add(rotated[2]),
        );
        i += 1;
    }
    out
}

fn box_prop_vertices(prop: &LevelBoxPropRecord) -> [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT] {
    let mut out = [WorldVertex::new(0, 0, 0); psx_level::BOX_PROP_VERTEX_COUNT];
    let mut i = 0usize;
    while i < prop.vertices.len() {
        let local = prop.vertices[i];
        let rotated = rotate_z_q12(
            rotate_y_q12(
                rotate_x_q12(
                    [local[0] as i32, local[1] as i32, local[2] as i32],
                    prop.pitch as u16,
                ),
                prop.yaw as u16,
            ),
            prop.roll as u16,
        );
        out[i] = WorldVertex::new(
            prop.x.saturating_add(rotated[0]),
            prop.y.saturating_add(rotated[1]),
            prop.z.saturating_add(rotated[2]),
        );
        i += 1;
    }
    out
}

fn box_prop_faces(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> [[WorldVertex; 4]; psx_level::BOX_PROP_FACE_COUNT] {
    let mut out = [[WorldVertex::new(0, 0, 0); 4]; psx_level::BOX_PROP_FACE_COUNT];
    let mut face = 0usize;
    while face < psx_level::BOX_PROP_FACE_COUNT {
        let mut corner = 0usize;
        while corner < 4 {
            out[face][corner] = vertices[BOX_PROP_FACE_VERTEX_INDICES[face][corner]];
            corner += 1;
        }
        face += 1;
    }
    out
}

fn box_prop_cull_bounds(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> (WorldVertex, i32) {
    let mut min_x = vertices[0].x;
    let mut max_x = vertices[0].x;
    let mut min_y = vertices[0].y;
    let mut max_y = vertices[0].y;
    let mut min_z = vertices[0].z;
    let mut max_z = vertices[0].z;
    for vertex in vertices {
        min_x = min_x.min(vertex.x);
        max_x = max_x.max(vertex.x);
        min_y = min_y.min(vertex.y);
        max_y = max_y.max(vertex.y);
        min_z = min_z.min(vertex.z);
        max_z = max_z.max(vertex.z);
    }
    let center = WorldVertex::new(
        min_x.saturating_add(max_x) / 2,
        min_y.saturating_add(max_y) / 2,
        min_z.saturating_add(max_z) / 2,
    );
    let radius = abs_delta_i32(max_x, min_x)
        .saturating_add(abs_delta_i32(max_y, min_y))
        .saturating_add(abs_delta_i32(max_z, min_z))
        / 2;
    (center, radius.max(32))
}

fn box_prop_floor_y(vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT]) -> i32 {
    let mut floor_y = vertices[0].y;
    for vertex in vertices {
        floor_y = floor_y.min(vertex.y);
    }
    floor_y
}

fn box_prop_debris_bounds(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> BoxPropDebrisBounds {
    let mut min_x = vertices[0].x;
    let mut max_x = vertices[0].x;
    let mut min_z = vertices[0].z;
    let mut max_z = vertices[0].z;
    for vertex in vertices {
        min_x = min_x.min(vertex.x);
        max_x = max_x.max(vertex.x);
        min_z = min_z.min(vertex.z);
        max_z = max_z.max(vertex.z);
    }
    BoxPropDebrisBounds {
        center_x: min_x.saturating_add(max_x) / 2,
        center_z: min_z.saturating_add(max_z) / 2,
        span_x: max_x.saturating_sub(min_x).max(64),
        span_z: max_z.saturating_sub(min_z).max(64),
    }
}

fn box_prop_aabb(prop: &LevelBoxPropRecord) -> (RoomPoint, RoomPoint) {
    let vertices = box_prop_vertices(prop);
    let mut min_x = vertices[0].x;
    let mut max_x = vertices[0].x;
    let mut min_y = vertices[0].y;
    let mut max_y = vertices[0].y;
    let mut min_z = vertices[0].z;
    let mut max_z = vertices[0].z;
    for vertex in vertices {
        min_x = min_x.min(vertex.x);
        max_x = max_x.max(vertex.x);
        min_y = min_y.min(vertex.y);
        max_y = max_y.max(vertex.y);
        min_z = min_z.min(vertex.z);
        max_z = max_z.max(vertex.z);
    }
    (
        RoomPoint::new(min_x, min_y, min_z),
        RoomPoint::new(max_x, max_y, max_z),
    )
}

fn box_prop_movement_break_trigger(
    input: CharacterMotorInput,
    config: CharacterMotorConfig,
    stamina_q12: i32,
) -> Option<u16> {
    let moving = input.move_x.raw() != 0 || input.move_z.raw() != 0 || input.walk != 0;
    if !moving {
        return None;
    }
    if input.sprint && stamina_q12 > 0 && config.run_speed > config.walk_speed {
        Some(box_prop_flags::BREAK_ON_RUN)
    } else {
        Some(box_prop_flags::BREAK_ON_WALK)
    }
}

fn box_prop_movement_probe_target(
    origin: RoomPoint,
    yaw: Angle,
    input: CharacterMotorInput,
    config: CharacterMotorConfig,
    trigger: u16,
    delta_vblanks: u16,
) -> RoomPoint {
    let base_speed = if trigger == box_prop_flags::BREAK_ON_RUN {
        config.run_speed
    } else {
        config.walk_speed
    };
    let speed = base_speed.saturating_mul(delta_vblanks.max(1).min(4) as i32);
    let dx = input.move_x.mul_i32(speed);
    let dz = input.move_z.mul_i32(speed);
    if dx != 0 || dz != 0 {
        return RoomPoint::new(
            origin.x.saturating_add(dx),
            origin.y,
            origin.z.saturating_add(dz),
        );
    }
    if input.walk == 0 || speed == 0 {
        return origin;
    }
    let signed_speed = if input.walk < 0 { -speed } else { speed };
    RoomPoint::new(
        origin.x.saturating_add(yaw.sin().mul_i32(signed_speed)),
        origin.y,
        origin.z.saturating_add(yaw.cos().mul_i32(signed_speed)),
    )
}

fn box_prop_break_impulse_from_delta(dx: i32, dz: i32) -> (i16, i16) {
    let denom = abs_i32_saturating(dx).saturating_add(abs_i32_saturating(dz));
    if denom <= 0 {
        return (0, 0);
    }
    let x = dx.saturating_mul(256) / denom;
    let z = dz.saturating_mul(256) / denom;
    (x.clamp(-256, 256) as i16, z.clamp(-256, 256) as i16)
}

fn box_prop_break_impulse_from_yaw(yaw: Angle) -> (i16, i16) {
    ((yaw.sin().raw() / 16) as i16, (yaw.cos().raw() / 16) as i16)
}

fn character_body_overlaps_aabb(
    position: RoomPoint,
    radius: i32,
    height: i32,
    min: RoomPoint,
    max: RoomPoint,
) -> bool {
    if max.y < position.y || min.y > position.y.saturating_add(height.max(1)) {
        return false;
    }
    let closest_x = position.x.clamp(min.x, max.x);
    let closest_z = position.z.clamp(min.z, max.z);
    let dx = position.x.saturating_sub(closest_x);
    let dz = position.z.saturating_sub(closest_z);
    square_i32_saturating(dx).saturating_add(square_i32_saturating(dz))
        <= square_i32_saturating(radius.max(0))
}

fn box_prop_intersects_attack_volume(
    origin: RoomPoint,
    yaw: Angle,
    config: CharacterMotorConfig,
    min: RoomPoint,
    max: RoomPoint,
) -> bool {
    let body_top = origin.y.saturating_add(config.height.max(1));
    if max.y < origin.y.saturating_sub(128) || min.y > body_top.saturating_add(128) {
        return false;
    }
    let center_x = min.x.saturating_add(max.x) / 2;
    let center_z = min.z.saturating_add(max.z) / 2;
    let dx = center_x.saturating_sub(origin.x);
    let dz = center_z.saturating_sub(origin.z);
    let sin_yaw = yaw.sin();
    let cos_yaw = yaw.cos();
    let forward = sin_yaw.mul_i32(dx).saturating_add(cos_yaw.mul_i32(dz));
    let lateral = cos_yaw.mul_i32(dx).saturating_sub(sin_yaw.mul_i32(dz));
    let prop_extent = abs_delta_i32(max.x, min.x).saturating_add(abs_delta_i32(max.z, min.z)) / 2;
    let reach = BOX_PROP_BREAK_ATTACK_REACH
        .saturating_add(config.radius.max(0))
        .saturating_add(prop_extent);
    let half_width = BOX_PROP_BREAK_ATTACK_WIDTH
        .saturating_add(config.radius.max(0))
        .saturating_add(prop_extent);
    forward >= -prop_extent && forward <= reach && abs_i32_saturating(lateral) <= half_width
}

fn rotate_x_q12(v: [i32; 3], angle_q12: u16) -> [i32; 3] {
    let angle = Angle::from_q12(angle_q12);
    let s = angle.sin().raw();
    let c = angle.cos().raw();
    [
        v[0],
        mul_q12_i32(v[1], c) - mul_q12_i32(v[2], s),
        mul_q12_i32(v[1], s) + mul_q12_i32(v[2], c),
    ]
}

fn rotate_y_q12(v: [i32; 3], angle_q12: u16) -> [i32; 3] {
    let angle = Angle::from_q12(angle_q12);
    let s = angle.sin().raw();
    let c = angle.cos().raw();
    [
        mul_q12_i32(v[0], c) + mul_q12_i32(v[2], s),
        v[1],
        -mul_q12_i32(v[0], s) + mul_q12_i32(v[2], c),
    ]
}

fn rotate_z_q12(v: [i32; 3], angle_q12: u16) -> [i32; 3] {
    let angle = Angle::from_q12(angle_q12);
    let s = angle.sin().raw();
    let c = angle.cos().raw();
    [
        mul_q12_i32(v[0], c) - mul_q12_i32(v[1], s),
        mul_q12_i32(v[0], s) + mul_q12_i32(v[1], c),
        v[2],
    ]
}

fn mul_q12_i32(value: i32, q12: i32) -> i32 {
    let whole = value >> Q12::FRACTIONAL_BITS;
    let fraction = value & (Q12::SCALE - 1);
    whole
        .saturating_mul(q12)
        .saturating_add(fraction.saturating_mul(q12) >> Q12::FRACTIONAL_BITS)
}

/// Draw one tinted cube per generated entity record. Cubes
/// reuse the room's first material with an override tint so
/// markers stand out from the surrounding geometry without
/// needing a dedicated texture upload.
fn draw_entity_markers(
    entities: &[EntityRecord],
    current_room: RoomIndex,
    materials: &[WorldRenderMaterial],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    if entities.is_empty() || materials.is_empty() {
        return;
    }
    // Reuse the room's first material so we don't need a
    // dedicated marker texture. Tint override picks up the
    // existing CLUT + tpage but recolours.
    let material = materials[0].texture.with_tint(MARKER_TINT);
    let opts = options.with_material_layer(material);
    const UVS: [(u8, u8); 4] = [(0, 0), (64, 0), (64, 64), (0, 64)];

    for entity in entities {
        if entity.room != current_room {
            continue;
        }
        let cx = entity.x;
        let cy = entity.y - MARKER_LIFT - MARKER_HALF;
        let cz = entity.z;
        let h = MARKER_HALF;

        let top = [
            WorldVertex::new(cx - h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy - h, cz + h),
            WorldVertex::new(cx - h, cy - h, cz + h),
        ];
        let bottom = [
            WorldVertex::new(cx - h, cy + h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz - h),
            WorldVertex::new(cx - h, cy + h, cz - h),
        ];
        let north = [
            WorldVertex::new(cx - h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy + h, cz - h),
            WorldVertex::new(cx - h, cy + h, cz - h),
        ];
        let south = [
            WorldVertex::new(cx + h, cy - h, cz + h),
            WorldVertex::new(cx - h, cy - h, cz + h),
            WorldVertex::new(cx - h, cy + h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz + h),
        ];
        let east = [
            WorldVertex::new(cx + h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy - h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz - h),
        ];
        let west = [
            WorldVertex::new(cx - h, cy - h, cz + h),
            WorldVertex::new(cx - h, cy - h, cz - h),
            WorldVertex::new(cx - h, cy + h, cz - h),
            WorldVertex::new(cx - h, cy + h, cz + h),
        ];

        for face in [top, bottom, north, south, east, west] {
            if let Some(projected) = camera.project_world_quad(face) {
                let _ = world.submit_textured_quad(triangles, projected, UVS, material, opts);
            }
        }
    }
}

fn draw_lock_target_indicator(target: RoomPoint, camera: WorldCamera, elapsed_tick: SimTick) {
    let Some(center) = camera.project_world(target) else {
        return;
    };

    let outer = TARGET_LOCK_OUTER;
    let inner = TARGET_LOCK_INNER;
    let half_width = TARGET_LOCK_TRI_HALF_WIDTH;
    let angle = Angle::per_frames(TARGET_LOCK_ROTATION_FRAMES).mul_tick(elapsed_tick);
    let triangles = [
        [
            target_screen_vertex(center, 0, -inner, angle),
            target_screen_vertex(center, -half_width, -outer, angle),
            target_screen_vertex(center, half_width, -outer, angle),
        ],
        [
            target_screen_vertex(center, 0, inner, angle),
            target_screen_vertex(center, half_width, outer, angle),
            target_screen_vertex(center, -half_width, outer, angle),
        ],
        [
            target_screen_vertex(center, -inner, 0, angle),
            target_screen_vertex(center, -outer, half_width, angle),
            target_screen_vertex(center, -outer, -half_width, angle),
        ],
        [
            target_screen_vertex(center, inner, 0, angle),
            target_screen_vertex(center, outer, -half_width, angle),
            target_screen_vertex(center, outer, half_width, angle),
        ],
    ];

    for triangle in triangles {
        draw_tri_flat_blended(
            triangle,
            TARGET_LOCK_RED.0,
            TARGET_LOCK_RED.1,
            TARGET_LOCK_RED.2,
            BlendMode::Average,
        );
    }
}

fn target_screen_vertex(center: ProjectedVertex, ox: i32, oy: i32, angle: Angle) -> (i16, i16) {
    let sin = angle.sin_q12();
    let cos = angle.cos_q12();
    let rx = ((ox.saturating_mul(cos)).saturating_sub(oy.saturating_mul(sin))) >> 12;
    let ry = ((ox.saturating_mul(sin)).saturating_add(oy.saturating_mul(cos))) >> 12;
    (
        clamp_i16((center.sx as i32).saturating_add(rx)),
        clamp_i16((center.sy as i32).saturating_add(ry)),
    )
}

fn draw_particle_emitter(
    emitter: ParticleEmitterRecord,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    elapsed_tick: SimTick,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    if emitter.flags & particle_emitter_flags::ENABLED == 0
        || emitter.max_particles == 0
        || emitter.lifetime_frames == 0
        || emitter.spawn_rate_q8 == 0
    {
        return 0;
    }

    let lifetime = emitter.lifetime_frames as u32;
    let steady_count = ((emitter.spawn_rate_q8 as u32)
        .saturating_mul(lifetime)
        .saturating_add(60 * 256 - 1))
        / (60 * 256);
    let count = (emitter.max_particles as u32)
        .min(PARTICLE_EMITTER_DRAW_CAP as u32)
        .min(steady_count.max(1));
    if count == 0 {
        return 0;
    }

    let mut submitted = 0usize;
    let mut i = 0u32;
    while i < count {
        let seed = particle_seed(
            emitter.room.to_usize() as u32,
            emitter.x as u32,
            emitter.z as u32,
            i,
        );
        let age = (elapsed_tick.as_u32() + (i * lifetime / count)) % lifetime;
        submitted += draw_particle_sample(
            emitter,
            camera,
            projector,
            depth_range,
            particle_material,
            seed,
            age as i32,
            lifetime as i32,
            ot,
            primitive_packets,
        );
        i += 1;
    }
    submitted
}

fn draw_particle_sample(
    emitter: ParticleEmitterRecord,
    camera: WorldCamera,
    projector: Option<LoadedWorldCameraGte>,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    seed: u32,
    age: i32,
    lifetime: i32,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let spawn_radius = emitter.spawn_radius as i32;
    let origin_x = emitter
        .x
        .saturating_add(particle_signed_spread(seed, spawn_radius));
    let origin_y = emitter.y.saturating_add(particle_signed_spread(
        seed.rotate_left(9),
        spawn_radius / 2,
    ));
    let origin_z = emitter
        .z
        .saturating_add(particle_signed_spread(seed.rotate_left(17), spawn_radius));
    let x = particle_axis_position(
        origin_x,
        emitter.base_velocity_q4[0],
        emitter.random_velocity_q4[0],
        emitter.acceleration_q4[0],
        age,
        seed.rotate_left(3),
    );
    let y = particle_axis_position(
        origin_y,
        emitter.base_velocity_q4[1],
        emitter.random_velocity_q4[1],
        emitter.acceleration_q4[1],
        age,
        seed.rotate_left(11),
    );
    let z = particle_axis_position(
        origin_z,
        emitter.base_velocity_q4[2],
        emitter.random_velocity_q4[2],
        emitter.acceleration_q4[2],
        age,
        seed.rotate_left(21),
    );
    let position = WorldVertex::new(x, y, z);
    let center = if let Some(projector) = projector {
        projector.project_world(position)
    } else {
        camera.project_world(position)
    };
    let Some(center) = center else {
        return 0;
    };

    let t_q8 = if lifetime <= 1 {
        255
    } else {
        ((age * 255) / (lifetime - 1)).clamp(0, 255)
    };
    let size = particle_lerp_u16(emitter.start_size, emitter.end_size, t_q8);
    let half = ((i32::from(size) * camera.projection.focal_length) / center.sz.max(1)).clamp(
        i32::from(PARTICLE_MIN_SCREEN_SIZE),
        i32::from(PARTICLE_MAX_SCREEN_SIZE),
    ) as i16;
    let tint = particle_lerp_rgb(emitter.start_color, emitter.end_color, t_q8);
    let blend = particle_blend_mode(emitter.blend_mode);
    let slot = depth_range.slot::<OT_DEPTH>(center.sz);
    draw_particle_quad(
        center,
        half,
        particle_material.with_tint(tint).with_blend_mode(blend),
        slot,
        ot,
        primitive_packets,
    )
}

fn draw_particle_quad(
    center: ProjectedVertex,
    half: i16,
    material: TextureMaterial,
    slot: psx_engine::DepthSlot,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    primitive_packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let left = clamp_i16(i32::from(center.sx).saturating_sub(i32::from(half)));
    let right = clamp_i16(i32::from(center.sx).saturating_add(i32::from(half)));
    let top = clamp_i16(i32::from(center.sy).saturating_sub(i32::from(half)));
    let bottom = clamp_i16(i32::from(center.sy).saturating_add(i32::from(half)));
    if left == right || top == bottom {
        return 0;
    }
    let quad = QuadTexturedMaterial::with_material(
        [(left, top), (right, top), (left, bottom), (right, bottom)],
        [
            (PARTICLE_TEXEL_U, PARTICLE_TEXEL_V),
            (PARTICLE_UV_MAX, PARTICLE_TEXEL_V),
            (PARTICLE_TEXEL_U, PARTICLE_UV_MAX),
            (PARTICLE_UV_MAX, PARTICLE_UV_MAX),
        ],
        material,
    );
    let Some(packet) = primitive_packets.push(quad) else {
        return 0;
    };
    ot.add_packet_slot(slot, packet);
    1
}

fn particle_axis_position(
    origin: i32,
    base_velocity_q4: i16,
    random_velocity_q4: u16,
    acceleration_q4: i16,
    age: i32,
    seed: u32,
) -> i32 {
    let random_velocity = particle_signed_spread(seed, random_velocity_q4 as i32);
    let velocity = i32::from(base_velocity_q4).saturating_add(random_velocity);
    let velocity_term = velocity.saturating_mul(age) >> 4;
    let acceleration_term = i32::from(acceleration_q4)
        .saturating_mul(age)
        .saturating_mul(age)
        >> 5;
    origin
        .saturating_add(velocity_term)
        .saturating_add(acceleration_term)
}

fn particle_seed(room: u32, x: u32, z: u32, index: u32) -> u32 {
    let mut value = room
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(x.rotate_left(7))
        .wrapping_add(z.rotate_left(17))
        .wrapping_add(index.wrapping_mul(0x85EB_CA6B));
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^ (value >> 16)
}

fn particle_signed_spread(seed: u32, spread: i32) -> i32 {
    if spread <= 0 {
        return 0;
    }
    let span = spread.saturating_mul(2).saturating_add(1) as u32;
    (seed % span) as i32 - spread
}

fn particle_lerp_u16(a: u16, b: u16, t_q8: i32) -> u16 {
    let inv = 255 - t_q8;
    (((i32::from(a) * inv) + (i32::from(b) * t_q8)) / 255).clamp(0, u16::MAX as i32) as u16
}

fn particle_lerp_rgb(a: [u8; 3], b: [u8; 3], t_q8: i32) -> (u8, u8, u8) {
    (
        particle_lerp_u8(a[0], b[0], t_q8),
        particle_lerp_u8(a[1], b[1], t_q8),
        particle_lerp_u8(a[2], b[2], t_q8),
    )
}

fn particle_lerp_u8(a: u8, b: u8, t_q8: i32) -> u8 {
    let inv = 255 - t_q8;
    (((i32::from(a) * inv) + (i32::from(b) * t_q8)) / 255).clamp(0, 255) as u8
}

const fn particle_blend_mode(mode: u8) -> BlendMode {
    match mode & 3 {
        1 => BlendMode::Add,
        2 => BlendMode::Subtract,
        3 => BlendMode::AddQuarter,
        _ => BlendMode::Average,
    }
}

fn draw_room_atmosphere_overlay(room: &LevelRoomRecord, elapsed_tick: SimTick) {
    if room.flags & room_flags::ATMOSPHERE_ENABLED == 0 {
        return;
    }
    let count = (room.atmosphere_density as u32).min(ATMOSPHERE_PARTICLE_MAX);
    if count == 0 {
        return;
    }
    let base_fall_q4 = room.atmosphere_fall_speed_q4.max(0) as i32;
    let base_wind_q4 = room.atmosphere_wind_speed_q4 as i32;
    let elapsed_vblanks = elapsed_tick.as_u32();
    let elapsed = elapsed_vblanks as i32;
    let mut i = 0u32;
    while i < count {
        let seed = atmosphere_seed(i);
        let layer = ((seed >> 4) & 3) as u32;
        let fall_q4 = base_fall_q4 + (layer as i32) * 3;
        let wind_q4 = base_wind_q4 + layer as i32;
        let base_x = (seed & 0x1ff) as i32;
        let base_y = ((seed >> 9) & 0x1ff) as i32;
        let drift_phase = ((elapsed_vblanks >> (2 + layer)) as i32 + ((seed >> 18) as i32)) & 31;
        let drift = drift_phase - 16;
        let x = wrap_atmosphere_axis(
            base_x + (elapsed.wrapping_mul(wind_q4) >> 4) + drift,
            ATMOSPHERE_WRAP_W,
        );
        let y = wrap_atmosphere_axis(
            base_y + (elapsed.wrapping_mul(fall_q4) >> 4),
            ATMOSPHERE_WRAP_H,
        );
        let size = 1 + (layer as i16 / 2);
        draw_atmosphere_particle(
            x,
            y,
            size,
            atmosphere_particle_tint(room.atmosphere_rgb, layer, seed),
        );
        i += 1;
    }
}

fn draw_atmosphere_particle(x: i16, y: i16, size: i16, tint: (u8, u8, u8)) {
    let lean = size + 1;
    draw_tri_flat_blended(
        [(x, y), (x + lean, y + 1), (x, y + size + 1)],
        tint.0,
        tint.1,
        tint.2,
        BlendMode::Average,
    );
}

fn atmosphere_particle_tint(base: [u8; 3], layer: u32, seed: u32) -> (u8, u8, u8) {
    let lift = ((layer * 10) + ((seed >> 22) & 7)) as i16;
    (
        tint_channel(base[0], lift),
        tint_channel(base[1], lift),
        tint_channel(base[2], lift),
    )
}

fn tint_channel(value: u8, delta: i16) -> u8 {
    (value as i16 + delta).clamp(0, 255) as u8
}

fn wrap_atmosphere_axis(value: i32, span: i32) -> i16 {
    (value.rem_euclid(span) - ATMOSPHERE_SCREEN_MARGIN) as i16
}

fn atmosphere_seed(index: u32) -> u32 {
    let mut x = index.wrapping_mul(0x9e37_79b9).wrapping_add(0x7f4a_7c15);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85eb_ca6b);
    x ^ (x >> 13)
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
    App::run(config, &mut scene);
}
