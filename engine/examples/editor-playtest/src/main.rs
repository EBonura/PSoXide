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
use psx_font::{upload_fonts, FontAtlas, FontSetVram};
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
use psx_vram::{
    upload_bytes, Clut, TexDepth, Tpage, VramAllocator, VramHandle, VramRect, VramRegionSource,
};

mod active_rooms;
mod box_props;
#[cfg(feature = "cd-stream-bench")]
mod cd_stream;
mod input;
mod model_rendering;
mod overlay;
mod runtime_schedule;
mod vram_upload;

use active_rooms::*;
use box_props::*;
use input::*;
use model_rendering::*;
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

/// Cooked sky panoramas occupy two side-by-side 4bpp pages. The
/// texture pixels are outside the double-buffered framebuffer and
/// model-atlas upload regions; each horizontal band gets a dedicated
/// CLUT row so the sky can spend 16 colours per altitude range.
const SKY_PANORAMA_CLUT_ENTRIES: u16 = 16;
const SKY_PANORAMA_PALETTE_BANDS: usize = 8;
const SKY_PANORAMA_WIDTH: u16 = 512;
const SKY_PANORAMA_HEIGHT: u16 = 256;
const SKY_PANORAMA_PAGE_WIDTH: u16 = 256;
const SKY_CYCLORAMA_GRID_POINTS_MAX: usize =
    (SKY_CYCLORAMA_COLUMNS_MAX as usize + 1) * (SKY_PANORAMA_PALETTE_BANDS + 1);
const SKY_CYCLORAMA_COLUMNS_MIN: u8 = 8;
const SKY_CYCLORAMA_COLUMNS_MAX: u8 = 12;

/// Runtime UI font slots. The cooked manifest compacts authored font choices
/// into these slots, so only fonts actually used by cooked UI text are uploaded.
const MAX_RUNTIME_UI_FONTS: usize = 4;
/// Resource-set key shared by every flow state: the UI font atlas is used by
/// the menus and the gameplay HUD, so it is acquired once and never torn down.
const UI_FONT_RESOURCE_KEY: u32 = 1;
/// Scratch for packing every UI font into one combined atlas before a single
/// `GP0(A0h)` upload. Sized for `MAX_RUNTIME_UI_FONTS` pages × 256-row atlases;
/// lives in BSS, not on the stack.
const FONT_PACK_SCRATCH_LEN: usize = MAX_RUNTIME_UI_FONTS * 64 * 256;
static mut FONT_PACK_SCRATCH: [u16; FONT_PACK_SCRATCH_LEN] = [0; FONT_PACK_SCRATCH_LEN];
const TARGET_LOCK_OUTER: i32 = 25;
const TARGET_LOCK_INNER: i32 = 13;
const TARGET_LOCK_TRI_HALF_WIDTH: i32 = 8;
const TARGET_LOCK_RED: (u8, u8, u8) = (225, 18, 24);
const TARGET_LOCK_ROTATION_FRAMES: u32 = 360;
static SHADOW_CIRCLE_BLOB: &[u8] = include_bytes!("../assets/shadow_circle_64.psxt");
/// Shadow (U=64) and particle (U=0) decals share one 4bpp page, allocated from
/// the unified allocator on first upload. UVs are page-relative, so only the
/// page base moves; the render quads are unchanged.
const SHADOW_TEXEL_U: u8 = 64;
const SHADOW_UV_MAX: u8 = SHADOW_TEXEL_U + 63;
const PARTICLE_TEXEL_U: u8 = 0;
const PARTICLE_TEXEL_V: u8 = 0;
const PARTICLE_TEXTURE_SIZE: u16 = 16;
const PARTICLE_UV_MAX: u8 = PARTICLE_TEXEL_U + PARTICLE_TEXTURE_SIZE as u8 - 1;
const PARTICLE_TEXTURE_HALFWORDS_PER_ROW: u16 = PARTICLE_TEXTURE_SIZE / 4;

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
    /// Allocator handle for the texture window/page this slot owns. `Empty` when
    /// the slot shares another slot's pixels (a clut-only variant) or is a
    /// session-persistent resource (model/sky) freed elsewhere.
    region: VramHandle,
    /// Allocator handle for this slot's CLUT. `Empty` if not separately owned.
    clut_region: VramHandle,
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
/// Find a free VRAM slot index, reusing holes left by eviction before growing
/// into fresh entries. Returns `None` when the slot table is full.
fn next_vram_slot() -> Option<usize> {
    unsafe { (0..MAX_RESIDENT_VRAM_ASSETS).find(|&i| VRAM_SLOTS[i].is_none()) }
}

/// Release slot `i`'s VRAM (texture window/page + CLUT) to the allocator, drop
/// its residency mark, and clear the slot for reuse. Caller must ensure the slot
/// is `ready`: a slot with a pending upload job must not be freed, or the async
/// writeback would land in a slot that has since been reused.
fn free_vram_slot(i: usize) {
    unsafe {
        if let Some(slot) = VRAM_SLOTS[i].take() {
            VRAM_ALLOCATOR.free(slot.region);
            VRAM_ALLOCATOR.free(slot.clut_region);
            let _ = RESIDENCY.mark_vram_evicted(slot.asset);
            VRAM_SLOT_COUNT = VRAM_SLOT_COUNT.saturating_sub(1);
            telemetry::counter(telemetry::counter::VRAM_SLOTS_FREED, 1);
        }
    }
}

/// Current room at the last eviction pass. Eviction only runs when the streamed
/// residency set shifts (the player crosses into a new room), keeping it off the
/// per-frame path.
static mut LAST_EVICT_ROOM: RoomIndex = INVALID_ROOM_INDEX;

/// True if any of the `count` desired rooms lists `asset` in its required VRAM set.
fn vram_asset_required(
    asset: AssetId,
    desired: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
    count: usize,
) -> bool {
    for &room in desired.iter().take(count) {
        if room == INVALID_ROOM_INDEX {
            continue;
        }
        if let Some(res) = ROOM_RESIDENCY.iter().find(|r| r.room == room) {
            if res.required_vram.iter().any(|&a| a == asset) {
                return true;
            }
        }
    }
    false
}

/// Free room-texture VRAM slots that no desired room still requires, returning
/// their window/CLUT to the allocator. Model atlases and the sky persist for the
/// session; only `ready` slots are freed so a pending upload's async writeback
/// cannot land in a slot that has since been reused.
fn evict_unreferenced_vram(desired: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT], count: usize) {
    for i in 0..MAX_RESIDENT_VRAM_ASSETS {
        let slot = match unsafe { VRAM_SLOTS[i] } {
            Some(s) if s.ready => s,
            _ => continue,
        };
        if !matches!(
            slot.clut_mode,
            VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
        ) {
            continue;
        }
        if !vram_asset_required(slot.asset, desired, count) {
            free_vram_slot(i);
        }
    }
}

/// CLUT-band rows the unified VRAM allocator manages, just past the back
/// buffer (Stage 1: only the shared font CLUT lands here).
const VRAM_CLUT_ROWS: usize = 16;
/// First VRAM row of the managed CLUT band.
const VRAM_CLUT_BASE_Y: u16 = 480;
/// The single owner of VRAM. Stage 1 routes fonts through it; later stages
/// fold in room textures, models, sky, shadow and particle.
static mut VRAM_ALLOCATOR: VramAllocator<ROOM_TPAGE_COUNT, VRAM_CLUT_ROWS> =
    VramAllocator::new(VRAM_CLUT_BASE_Y);
/// Set once the still-hardcoded VRAM regions are reserved in `VRAM_ALLOCATOR`.
static mut VRAM_REGIONS_RESERVED: bool = false;
/// Handles for the combined UI-font upload, kept for teardown.
static mut VRAM_FONT_SET: Option<FontSetVram> = None;

/// Reserve the framebuffer and every region still owned by legacy hardcoded
/// uploads, so the allocator places fonts in the remaining free VRAM without
/// collision. Each reserved region migrates into managed allocation in a later
/// stage.
fn reserve_static_vram_regions(alloc: &mut VramAllocator<ROOM_TPAGE_COUNT, VRAM_CLUT_ROWS>) {
    // Double-buffered framebuffer.
    alloc.reserve_rect(VramRect::new(0, 0, 320, 480));
    // Room-material window band (allocated via the unified allocator).
    alloc.reserve_room_band(ROOM_TPAGE_BASE_X, 0);
    // Column between the framebuffer and the model-atlas region. Model atlases,
    // the sky panorama, and shadow/particle decals are all allocated dynamically
    // (rows 256 and 0); reserving this gap keeps model atlases at their historical
    // x=384 base.
    alloc.reserve_rect(VramRect::new(
        320,
        MODEL_TPAGE.y(),
        MODEL_TPAGE.x() - 320,
        256,
    ));
}

const VRAM_UPLOAD_QUEUE_CAP: usize = 8;
const VRAM_UPLOAD_ROWS_PER_BACKGROUND_TICK: u16 = 8;
const UI_TEXTURE_UPLOAD_ROW_BUDGET: u16 = ROOM_TILE_TEXELS;
const UI_TEXTURE_UPLOAD_MAX_STEPS: u8 = 8;
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

    fn is_idle(&self) -> bool {
        let mut i = 0usize;
        while i < self.jobs.len() {
            if self.jobs[i].active {
                return false;
            }
            i += 1;
        }
        true
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

    fn vram_uploads_idle(self) -> bool {
        unsafe { VRAM_UPLOAD_QUEUE.is_idle() }
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
        unsafe {
            if !VRAM_REGIONS_RESERVED {
                reserve_static_vram_regions(&mut VRAM_ALLOCATOR);
                VRAM_REGIONS_RESERVED = true;
            }
            if self.ui_fonts[0].is_none() && !UI_FONTS.is_empty() {
                VRAM_FONT_SET = upload_fonts(
                    UI_FONTS,
                    &mut VRAM_ALLOCATOR,
                    &mut FONT_PACK_SCRATCH,
                    &mut self.ui_fonts,
                );
            }
        }
    }

    /// Release the shared resource set. Dormant in this project's flow (the
    /// font set never leaves the active set) but correct: free the VRAM and
    /// clear the atlases so a re-entry re-acquires cleanly.
    fn on_exit_state(&mut self, _state: SceneStateRef, _ctx: &mut Ctx) {
        unsafe {
            if let Some(set) = VRAM_FONT_SET.take() {
                VRAM_ALLOCATOR.free(set.pages);
                VRAM_ALLOCATOR.free(set.clut);
                self.ui_fonts = [const { None }; MAX_RUNTIME_UI_FONTS];
            }
        }
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
    let grid_stride = columns + 1;

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
        let row_base = row * grid_stride;
        for column in 0..=columns {
            let dir = [
                clamp_i16(-mul_q12_i32(yaw_sin[column], pitch_cos[row])),
                clamp_i16(pitch_sin[row]),
                clamp_i16(-mul_q12_i32(yaw_cos[column], pitch_cos[row])),
            ];
            projected_grid[row_base + column] = sky_projector
                .project(dir)
                .map(|(sx, sy)| (sx.clamp(-512, 831), sy.clamp(-256, 495)));
        }
    }

    let mut column_tpage_word = [0u16; SKY_CYCLORAMA_COLUMNS_MAX as usize];
    let mut column_u0 = [0u8; SKY_CYCLORAMA_COLUMNS_MAX as usize];
    let mut column_u1 = [0u8; SKY_CYCLORAMA_COLUMNS_MAX as usize];
    for column in 0..columns {
        let page = sky_panorama_page_for_column(column, columns);
        column_tpage_word[column] = sky_panorama_tpage_word(page);
        column_u0[column] = sky_panorama_local_u(
            sky_coord_for_step(column, columns, SKY_PANORAMA_WIDTH),
            page,
        );
        column_u1[column] = sky_panorama_local_u(
            sky_coord_for_step(column + 1, columns, SKY_PANORAMA_WIDTH),
            page,
        );
    }

    for row in 0..rows {
        let row_base = row * grid_stride;
        let next_row_base = row_base + grid_stride;
        let v0 = sky_uv_for_step(row, rows, SKY_PANORAMA_HEIGHT);
        let v1 = sky_uv_for_step(row + 1, rows, SKY_PANORAMA_HEIGHT);
        let clut_word = sky_panorama_clut_word(sky_panorama_clut_band_for_row(row, rows));
        for column in 0..columns {
            let material =
                TextureMaterial::opaque(clut_word, column_tpage_word[column], (0x80, 0x80, 0x80))
                    .with_raw_texture(true)
                    .with_dither(true);
            let Some(p0) = projected_grid[row_base + column] else {
                continue;
            };
            let Some(p1) = projected_grid[row_base + column + 1] else {
                continue;
            };
            let Some(p2) = projected_grid[next_row_base + column] else {
                continue;
            };
            let Some(p3) = projected_grid[next_row_base + column + 1] else {
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
                [
                    (column_u0[column], v0),
                    (column_u1[column], v0),
                    (column_u0[column], v1),
                    (column_u1[column], v1),
                ],
                material,
            );
            if let Some(packet) = primitive_packets.push(quad) {
                ot.add_packet_slot(SKY_OT_SLOT, packet);
            }
        }
    }
}

fn sky_quad_outside_screen(points: [(i16, i16); 4]) -> bool {
    let min_x = points[0]
        .0
        .min(points[1].0)
        .min(points[2].0)
        .min(points[3].0);
    let max_x = points[0]
        .0
        .max(points[1].0)
        .max(points[2].0)
        .max(points[3].0);
    let min_y = points[0]
        .1
        .min(points[1].1)
        .min(points[2].1)
        .min(points[3].1);
    let max_y = points[0]
        .1
        .max(points[1].1)
        .max(points[2].1)
        .max(points[3].1);
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

/// Sky panorama placement, filled by `ensure_sky_panorama_uploaded` from the
/// unified allocator: two contiguous 4bpp page words + one CLUT word per band.
static mut SKY_PAGE_TPAGE_WORDS: [u16; 2] = [0; 2];
static mut SKY_CLUT_WORDS: [u16; SKY_PANORAMA_PALETTE_BANDS] = [0; SKY_PANORAMA_PALETTE_BANDS];

fn sky_panorama_tpage_word(page: usize) -> u16 {
    unsafe { SKY_PAGE_TPAGE_WORDS[page.min(1)] }
}

fn sky_panorama_clut_word(band: usize) -> u16 {
    unsafe { SKY_CLUT_WORDS[band.min(SKY_PANORAMA_PALETTE_BANDS - 1)] }
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

#[cfg(feature = "cd-stream-bench")]
fn room_backdrop_textures_ready(record: &LevelRoomRecord) -> bool {
    sky_panorama_texture_ready(record.sky) & far_vista_textures_ready(record.far_vista)
}

#[cfg(feature = "cd-stream-bench")]
fn sky_panorama_texture_ready(sky: LevelSkyRecord) -> bool {
    if sky.flags & sky_flags::ENABLED == 0 {
        return true;
    }
    let Some(asset) = find_asset_of_kind(ASSETS, sky.cloud_layer.texture_asset, AssetKind::Texture)
    else {
        return true;
    };
    ensure_sky_panorama_uploaded(asset.id, asset.bytes).is_some()
}

#[cfg(feature = "cd-stream-bench")]
fn far_vista_textures_ready(vista: LevelFarVistaRecord) -> bool {
    if vista.flags & far_vista_flags::ENABLED == 0 || vista.flags & far_vista_flags::TEXTURED == 0 {
        return true;
    }
    let segments = vista.segments.clamp(3, 16);
    let mut ready = true;
    let mut segment = 0u8;
    while segment < segments {
        if let Some(asset_id) = far_vista_panel_asset(vista, segment, segments) {
            if let Some(asset) = find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture) {
                if ensure_texture_uploaded_with_clut_mode(
                    asset.id,
                    asset.bytes,
                    VramSlotClutMode::TransparentZero,
                )
                .is_none()
                {
                    ready = false;
                }
            }
        }
        segment += 1;
    }
    ready
}

fn vram_slot_texture_size_u8(size: u16) -> u8 {
    size.min(u16::from(u8::MAX)) as u8
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

    fn prewarm_visible_cell_caches(&mut self) {
        if self.current_collision_room.is_none() {
            return;
        }
        let camera = self.render_camera;
        let active_draw_order = active_room_draw_order(
            &self.active_rooms,
            camera,
            &self.portal_visibility,
            self.room_index,
            cached_room_draw_order_mode(),
        );
        let player = self.motor.position();
        let global_visibility_anchor = player;

        telemetry::stage_begin(telemetry::stage::ROOM_VISIBLE_LIST);
        for &active_slot in &active_draw_order {
            if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                continue;
            }
            let active_slot = active_slot as usize;
            let Some(active) = self.active_rooms[active_slot] else {
                continue;
            };
            if !self.portal_visibility_draws_room(active.index) {
                continue;
            }
            let visibility_anchor = RoomPoint::new(
                global_visibility_anchor.x.saturating_sub(active.offset_x),
                player.y,
                global_visibility_anchor.z.saturating_sub(active.offset_z),
            );
            let room_camera = camera_for_room(camera, active);
            let _ = self.cached_precomputed_visible_cells(
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
        }
        telemetry::stage_end(telemetry::stage::ROOM_VISIBLE_LIST);
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

        let required_cells = room_visibility_candidate_count(room_index)?;
        let mut first = self.visible_cell_cache_cursor;
        if MAX_ACTIVE_VISIBLE_CELLS.saturating_sub(first) < required_cells {
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
fn room_visibility_candidate_count(room_index: RoomIndex) -> Option<usize> {
    ROOM_VISIBILITY
        .iter()
        .find(|visibility| visibility.room == room_index)
        .map(|visibility| visibility.cell_count as usize)
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
    let half = sector_size >> 1;
    let center = WorldVertex::new(
        (cell.x as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
        cell.min_y.saturating_add(cell.max_y) / 2,
        (cell.z as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
    );
    let half_height = ((cell.max_y - cell.min_y).abs() >> 1).max(half);
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
    let half = sector_size >> 1;
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
    let half = sector_size >> 1;
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
    let margin = ROOM_VISIBLE_CELL_CAMERA_MARGIN.max(sector_size >> 2);
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
    _visibility: &RuntimePortalVisibility,
    _current_room: RoomIndex,
    _index: RoomIndex,
) -> bool {
    // Reachability draw: the draw-order builders only pass rooms from the active
    // window (the camera ring), so every one is drawn -- no frustum/far-distance
    // cull gates room drawing here. Per-cell frustum + per-polygon backface and
    // screen culling still trim the off-screen geometry. This is the draw-order
    // twin of `portal_visibility_draws_room`; the reachability-draw rework
    // flipped that one but missed this, so a reachable-but-not-frustum-visible
    // room (e.g. the room behind the player) was dropped from the draw order.
    true
}

fn active_room_sort_depth(active: ActiveRuntimeRoom, camera: WorldCamera) -> i32 {
    let sector_size = active.sector_size.max(1);
    let center_x = active
        .offset_x
        .saturating_add((active.width as i32).saturating_mul(sector_size) >> 1);
    let center_z = active
        .offset_z
        .saturating_add((active.depth as i32).saturating_mul(sector_size) >> 1);
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

fn room_origin_x(record: &LevelRoomRecord) -> i32 {
    record.origin_x.saturating_mul(record.sector_size)
}

fn room_origin_z(record: &LevelRoomRecord) -> i32 {
    record.origin_z.saturating_mul(record.sector_size)
}

/// Vertical origin of a room in engine units. Unlike X/Z (`origin_*` in
/// sectors), `origin_y` is already stored in engine units, so it is used
/// directly. Drives Y rebasing across room transitions for stacked floors.
fn room_origin_y(record: &LevelRoomRecord) -> i32 {
    record.origin_y
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
    let visibility_index =
        room_index_containing_global_from(current_index, camera_global).unwrap_or(current_index);
    let visibility_view = if visibility_index == current_index {
        view
    } else {
        ActiveRoomView {
            position: global_to_local_room_point(visibility_index, camera_global),
            ..view
        }
    };
    PortalVisibilitySpace {
        room: visibility_index,
        view: visibility_view,
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

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
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

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
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
        point.y.saturating_add(room_origin_y(record)),
        point.z.saturating_add(room_origin_z(record)),
    )
}

fn global_to_local_room_point(room: RoomIndex, point: RoomPoint) -> RoomPoint {
    let Some(record) = ROOMS.get(room.to_usize()) else {
        return point;
    };
    RoomPoint::new(
        point.x.saturating_sub(room_origin_x(record)),
        point.y.saturating_sub(room_origin_y(record)),
        point.z.saturating_sub(room_origin_z(record)),
    )
}

fn camera_for_room(camera: WorldCamera, active: ActiveRuntimeRoom) -> WorldCamera {
    WorldCamera::from_basis(
        camera.projection,
        WorldVertex::new(
            camera.position.x.saturating_sub(active.offset_x),
            camera.position.y.saturating_sub(active.offset_y),
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

/// Shadow and particle decals share one 4bpp page (shadow at U=64, particle at
/// U=0). Allocated once from the unified allocator on first decal upload.
static mut SHADOW_PARTICLE_PAGE: Option<Tpage> = None;

fn shadow_particle_page() -> Option<Tpage> {
    unsafe {
        if SHADOW_PARTICLE_PAGE.is_none() {
            let (tpage, _region) = VRAM_ALLOCATOR.alloc_page_run(1, TexDepth::Bit4, 0)?;
            SHADOW_PARTICLE_PAGE = Some(tpage);
        }
        SHADOW_PARTICLE_PAGE
    }
}

fn upload_shadow_texture() -> Option<TextureMaterial> {
    let texture = Texture::from_bytes(SHADOW_CIRCLE_BLOB).ok()?;
    if texture.width() != 64 || texture.height() != 64 || texture.clut_entries() != 16 {
        return None;
    }

    let page = shadow_particle_page()?;
    let (clut, _clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
    upload_bytes(
        VramRect::new(
            page.x() + u16::from(SHADOW_TEXEL_U) / 4,
            page.y(),
            texture.halfwords_per_row(),
            texture.height(),
        ),
        texture.pixel_bytes(),
    );
    upload_clut(
        VramRect::new(clut.x(), clut.y(), texture.clut_entries(), 1),
        texture.clut_bytes(),
    );

    Some(
        TextureMaterial::blended(
            clut.uv_clut_word(),
            page.uv_tpage_word(0),
            (0x80, 0x80, 0x80),
            BlendMode::Average,
        )
        .with_raw_texture(true),
    )
}

fn upload_particle_texture() -> Option<TextureMaterial> {
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

    let page = shadow_particle_page()?;
    let (clut_pos, _clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(16)? };
    upload_bytes(
        VramRect::new(
            page.x() + u16::from(PARTICLE_TEXEL_U) / 4,
            page.y(),
            PARTICLE_TEXTURE_HALFWORDS_PER_ROW,
            PARTICLE_TEXTURE_SIZE,
        ),
        &pixels,
    );
    upload_clut(VramRect::new(clut_pos.x(), clut_pos.y(), 16, 1), &clut);

    Some(TextureMaterial::blended(
        clut_pos.uv_clut_word(),
        page.uv_tpage_word(0),
        (0x80, 0x80, 0x80),
        BlendMode::Average,
    ))
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

fn ensure_ui_texture_uploaded(asset_id: AssetId, asset_bytes: &'static [u8]) -> Option<VramSlot> {
    let texture = Texture::from_bytes(asset_bytes).ok()?;
    let clut_mode = if texture.index_zero_transparent() {
        VramSlotClutMode::TransparentZero
    } else {
        VramSlotClutMode::OpaqueZero
    };
    if let Some(slot) = find_vram_slot(asset_id, clut_mode) {
        return Some(slot);
    }

    let use_large_ui_upload = texture.width() > ROOM_TILE_TEXELS
        || texture.height() > ROOM_TILE_TEXELS
        || room_texture_window_size(texture.width()).is_none()
        || room_texture_window_size(texture.height()).is_none();
    let _ = if use_large_ui_upload {
        ensure_large_ui_texture_uploaded_with_clut_mode(asset_id, asset_bytes, clut_mode)
    } else {
        ensure_texture_uploaded_with_clut_mode(asset_id, asset_bytes, clut_mode)
    };
    let mut steps = 0u8;
    while pending_vram_upload(asset_id, clut_mode) && steps < UI_TEXTURE_UPLOAD_MAX_STEPS {
        unsafe {
            VRAM_UPLOAD_QUEUE.step(UI_TEXTURE_UPLOAD_ROW_BUDGET);
        }
        steps = steps.saturating_add(1);
    }

    find_vram_slot(asset_id, clut_mode)
}

fn ensure_large_ui_texture_uploaded_with_clut_mode(
    asset_id: AssetId,
    asset_bytes: &'static [u8],
    clut_mode: VramSlotClutMode,
) -> Option<VramSlot> {
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
    if texture.clut_entries() != 16
        || texture.width() == 0
        || texture.width() > 256
        || texture.height() == 0
        || texture.height() > 256
    {
        return None;
    }
    let texture_width_halfwords = texture.halfwords_per_row();
    if texture_width_halfwords > ROOM_TPAGE_STRIDE_HW {
        return None;
    }
    let expected_pixel_bytes = usize::from(texture_width_halfwords)
        .saturating_mul(usize::from(texture.height()))
        .saturating_mul(2);
    if texture.pixel_bytes().len() != expected_pixel_bytes {
        return None;
    }

    let idx = next_vram_slot()?;
    let (tpage, region) = unsafe { VRAM_ALLOCATOR.alloc_room_page()? };
    let tpage_x = tpage.x();
    let (clut, clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
    let slot = VramSlot {
        asset: asset_id,
        clut_mode,
        ready: false,
        clut_word: clut.uv_clut_word(),
        tpage_word: tpage.uv_tpage_word(0),
        texture_window: TextureWindow::NONE,
        texture_width: texture.width(),
        texture_height: texture.height(),
        region,
        clut_region,
    };

    unsafe {
        VRAM_SLOTS[idx] = Some(slot);
        VRAM_SLOT_COUNT += 1;
        if !VRAM_UPLOAD_QUEUE.push(VramUploadJob {
            active: true,
            slot_index: idx as u16,
            asset: asset_id,
            clut_mode,
            kind: VramUploadKind::TextureAndClut,
            bytes: Some(asset_bytes),
            texture_x: tpage_x,
            texture_y: SHARED_TPAGE.y(),
            texture_width_halfwords,
            texture_height_rows: texture.height(),
            next_texture_row: 0,
            clut_x: clut.x(),
            clut_y: clut.y(),
            clut_entries: texture.clut_entries(),
            clut_uploaded: false,
        }) {
            VRAM_SLOTS[idx] = None;
            VRAM_SLOT_COUNT -= 1;
            return None;
        }
    }

    None
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
    let idx = next_vram_slot()?;

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

    if let Some(shared_texture) = find_room_texture_vram_slot(asset_id) {
        let (clut, clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
        let slot = VramSlot {
            asset: asset_id,
            clut_mode,
            ready: false,
            clut_word: clut.uv_clut_word(),
            tpage_word: shared_texture.tpage_word,
            texture_window: shared_texture.texture_window,
            texture_width: shared_texture.texture_width,
            texture_height: shared_texture.texture_height,
            // Shared pixels are owned by the original slot; this variant only
            // owns its own CLUT.
            region: VramHandle::Empty,
            clut_region,
        };
        unsafe {
            VRAM_SLOTS[idx] = Some(slot);
            VRAM_SLOT_COUNT += 1;
            if !VRAM_UPLOAD_QUEUE.push(VramUploadJob {
                active: true,
                slot_index: idx as u16,
                asset: asset_id,
                clut_mode,
                kind: VramUploadKind::ClutOnly,
                bytes: Some(asset_bytes),
                texture_x: 0,
                texture_y: 0,
                texture_width_halfwords: 0,
                texture_height_rows: 0,
                next_texture_row: 0,
                clut_x: clut.x(),
                clut_y: clut.y(),
                clut_entries: texture.clut_entries(),
                clut_uploaded: false,
            }) {
                VRAM_SLOTS[idx] = None;
                VRAM_SLOT_COUNT -= 1;
                return None;
            }
        }
        return None;
    }

    // Pack room materials on the GP0(E2) 8-texel grid inside 4bpp
    // tpages. A 32x32 texture now consumes a 32x32 window instead of
    // burning a whole old 64x64 cell.
    let (tpage, placement, region) = unsafe {
        VRAM_ALLOCATOR.alloc_window(u16::from(texture_width), u16::from(texture_height))?
    };
    let tpage_x = tpage.x();
    let texture_x = tpage_x.checked_add(u16::from(placement.origin_u()) / 4)?;
    let texture_y = SHARED_TPAGE
        .y()
        .checked_add(u16::from(placement.origin_v()))?;

    let (clut, clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
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
        region,
        clut_region,
    };

    unsafe {
        VRAM_SLOTS[idx] = Some(slot);
        VRAM_SLOT_COUNT += 1;
        if !VRAM_UPLOAD_QUEUE.push(VramUploadJob {
            active: true,
            slot_index: idx as u16,
            asset: asset_id,
            clut_mode,
            kind: VramUploadKind::TextureAndClut,
            bytes: Some(asset_bytes),
            texture_x,
            texture_y,
            texture_width_halfwords,
            texture_height_rows,
            next_texture_row: 0,
            clut_x: clut.x(),
            clut_y: clut.y(),
            clut_entries: texture.clut_entries(),
            clut_uploaded: false,
        }) {
            VRAM_SLOTS[idx] = None;
            VRAM_SLOT_COUNT -= 1;
            return None;
        }
    }

    None
}

fn prop_texture_slot(texture_asset: AssetId) -> Option<VramSlot> {
    let clut_mode = VramSlotClutMode::TransparentZero;
    if let Some(slot) = find_vram_slot(texture_asset, clut_mode) {
        return Some(slot);
    }
    let asset = find_asset_of_kind(ASSETS, texture_asset, AssetKind::Texture)?;
    ensure_texture_uploaded_with_clut_mode(asset.id, asset.bytes, clut_mode)
}

#[cfg(feature = "cd-stream-bench")]
fn room_prop_textures_ready(room: RoomIndex) -> bool {
    let mut ready = true;

    for prop in IMAGE_PROPS {
        if prop.room == room && prop_texture_slot(prop.texture_asset).is_none() {
            ready = false;
        }
    }

    for prop in BOX_PROPS {
        if prop.room != room {
            continue;
        }
        let mut face = 0usize;
        while face < psx_level::BOX_PROP_FACE_COUNT {
            if let Some(texture_asset) = prop.texture_assets[face] {
                if prop_texture_slot(texture_asset).is_none() {
                    ready = false;
                }
            }
            face += 1;
        }
    }

    ready
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
    let idx = next_vram_slot()?;
    let expected_pixel_bytes = (texture.halfwords_per_row() as usize)
        .saturating_mul(texture.height() as usize)
        .saturating_mul(2);
    if texture.pixel_bytes().len() != expected_pixel_bytes {
        return None;
    }

    let clut_row_bytes = usize::from(SKY_PANORAMA_CLUT_ENTRIES) * 2;
    if texture.clut_bytes().len() != clut_row_bytes * SKY_PANORAMA_PALETTE_BANDS {
        return None;
    }
    // Two contiguous 4bpp pages (the 512-texel panorama) + one CLUT per band,
    // all from the unified allocator.
    let (left_tpage, _page_region) =
        unsafe { VRAM_ALLOCATOR.alloc_page_run(2, TexDepth::Bit4, 256)? };
    let right_tpage = Tpage::new(left_tpage.x() + 64, left_tpage.y(), TexDepth::Bit4);
    let mut sky_cluts = [Clut::new(0, 0); SKY_PANORAMA_PALETTE_BANDS];
    for dst in sky_cluts.iter_mut() {
        let (clut, _clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(SKY_PANORAMA_CLUT_ENTRIES)? };
        *dst = clut;
    }
    unsafe {
        SKY_PAGE_TPAGE_WORDS = [left_tpage.uv_tpage_word(0), right_tpage.uv_tpage_word(0)];
        for (band, clut) in sky_cluts.iter().enumerate() {
            SKY_CLUT_WORDS[band] = clut.uv_clut_word();
        }
    }

    telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
    telemetry::counter(telemetry::counter::ROOM_TEXTURE_UPLOADS, 1);
    upload_bytes(
        VramRect::new(
            left_tpage.x(),
            left_tpage.y(),
            texture.halfwords_per_row(),
            texture.height(),
        ),
        texture.pixel_bytes(),
    );
    for (band, clut) in sky_cluts.iter().enumerate() {
        let offset = band * clut_row_bytes;
        upload_model_clut(
            VramRect::new(clut.x(), clut.y(), SKY_PANORAMA_CLUT_ENTRIES, 1),
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
        tpage_word: sky_panorama_tpage_word(0),
        texture_window: TextureWindow::NONE,
        texture_width: texture.width(),
        texture_height: texture.height(),
        // Sky is session-persistent (2 pages + 8 CLUTs); not freed via slot eviction.
        region: VramHandle::Empty,
        clut_region: VramHandle::Empty,
    };
    unsafe {
        VRAM_SLOTS[idx] = Some(slot);
        VRAM_SLOT_COUNT += 1;
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

    let idx = next_vram_slot()?;
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

    // Placement comes only from the unified allocator: an 8bpp page run at row 256
    // plus a 256-entry CLUT (16 contiguous 16-px slots in the managed band).
    let (tpage, region) = unsafe { VRAM_ALLOCATOR.alloc_model_slot(texture_halfwords_per_row)? };
    let (clut, clut_region) = unsafe { VRAM_ALLOCATOR.alloc_clut(texture.clut_entries())? };
    telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
    telemetry::counter(telemetry::counter::MODEL_ATLAS_UPLOADS, 1);
    let pix_rect = VramRect::new(
        tpage.x(),
        tpage.y(),
        texture_halfwords_per_row,
        texture_height,
    );
    upload_bytes(pix_rect, texture.pixel_bytes());

    let clut_rect = VramRect::new(clut.x(), clut.y(), texture.clut_entries(), 1);
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
        clut_word: clut.uv_clut_word(),
        tpage_word: tpage.uv_tpage_word(0),
        texture_window: TextureWindow::NONE,
        texture_width,
        texture_height,
        // Model atlases are session-persistent; handles stored but not evicted.
        region,
        clut_region,
    };

    unsafe {
        VRAM_SLOTS[idx] = Some(slot);
        VRAM_SLOT_COUNT += 1;
        let _ = RESIDENCY.mark_vram_resident(asset_id);
    }
    Some(slot)
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
        let Some(slot) = prop_texture_slot(prop.texture_asset) else {
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
                let _ = world.submit_textured_gouraud_triangle_prescreened_u8(
                    triangles,
                    [projected[0], projected[1], projected[2]],
                    [uvs[0], uvs[1], uvs[2]],
                    [colors[0], colors[1], colors[2]],
                    material,
                    opts,
                );
                let _ = world.submit_textured_gouraud_triangle_prescreened_u8(
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
            let _ = world.submit_textured_gouraud_triangle_prescreened_u8(
                triangles,
                [projected[0], projected[1], projected[2]],
                [uvs[0], uvs[1], uvs[2]],
                [colors[0], colors[1], colors[2]],
                material,
                opts,
            );
            let _ = world.submit_textured_gouraud_triangle_prescreened_u8(
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

fn image_prop_depth_bias(width: u16, height: u16) -> i32 {
    IMAGE_PROP_DEPTH_BIAS.saturating_add((width.max(height) as i32) >> 1)
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
        let half_width = (width as i32) >> 1;
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

    let half_width = (width as i32) >> 1;
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
        spawn_radius >> 1,
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
        let size = 1 + ((layer as i16) >> 1);
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
