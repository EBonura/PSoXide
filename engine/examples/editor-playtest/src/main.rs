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
#[cfg(feature = "cd-stream-bench")]
use psx_engine::CompactCollisionRoom;
use psx_engine::{
    button, compute_joint_world_transform, telemetry, Angle, App, CachedRoomCell,
    CachedRoomSurface, CharacterCollision, CharacterCollisionCylinder, CharacterCollisionRoom,
    CharacterMotorAnim, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, Config,
    Ctx, CullMode, DepthBand, DepthPolicy, DepthRange, JointViewTransform, JointWorldTransform,
    LocalToWorldScale, Mat3I16, MaterialTint, OtFrame, PointLightSample, PrimitivePacketArena,
    PrimitivePacketScratch, PrimitiveSink, ProjectedVertex, Rgb8, RoomPoint, RoomRender,
    RuntimeCollisionRoom, RuntimeRoom, Scene, TexturedModelGeometry, TexturedModelRenderFace,
    TexturedModelRenderStats, ThirdPersonCameraConfig, ThirdPersonCameraInput,
    ThirdPersonCameraState, ThirdPersonCameraTarget, VisualPacing, WorldCamera, WorldProjection,
    WorldRenderMaterial, WorldRenderPass, WorldSurfaceLighting, WorldSurfaceOptions,
    WorldSurfaceSample, WorldTriCommand, WorldVertex, Q12, Q8,
};
use psx_engine::{
    cached_room_cells_from_level_records, cached_room_surfaces_from_level_records,
    cached_room_vertices_from_level_records, draw_indexed_cached_room_vertex_lit_visible_cells,
    draw_room_vertex_lit,
};
#[cfg(feature = "world-grid-visible")]
use psx_engine::{
    draw_room_vertex_lit_visible_cells, GridVisibility, GridVisibilityStats, GridVisibleCell,
};
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{
    draw_line_mono, draw_quad_textured_material, draw_tri_flat_blended,
    material::{BlendMode, TextureMaterial, TextureWindow},
    ot::OrderingTable,
    prim::TriTextured,
    VideoMode,
};
use psx_level::{
    equipment_flags, far_vista_flags, find_asset_of_kind, image_prop_flags, room_flags, sky_flags,
    AssetId, AssetKind, CharacterAnimationAction, EntityRecord, LevelCameraRecord,
    LevelCharacterRecord, LevelChunkRecord, LevelFarVistaRecord, LevelImagePropRecord,
    LevelMaterialRecord, LevelMaterialSidedness, LevelModelFrameBoundsRecord, LevelModelRecord,
    LevelModelSocketRecord, LevelRoomRecord, LevelSkyRecord, ModelClipIndex, ModelClipTableIndex,
    ModelIndex, ModelSocketIndex, OptionalModelClipIndex, ResidencyManager, RoomIndex,
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
mod vram_upload;

use input::*;
use overlay::*;
use vram_upload::*;

// Placeholder manifests reference unused statics; populated
// manifests reference all of them. Quiet either side here.
#[allow(dead_code, unused_imports)]
mod generated {
    include!(env!("PSXED_PLAYTEST_MANIFEST"));
}

use generated::{
    ASSETS, CHARACTERS, ENTITIES, EQUIPMENT, IMAGE_PROPS, LIGHTS, MATERIALS, MODELS, MODEL_CLIPS,
    MODEL_CLIP_BOUNDS, MODEL_FRAME_BOUNDS, MODEL_INSTANCES, MODEL_SOCKETS, PLAYER_CONTROLLER,
    PLAYER_SPAWN, ROOMS, ROOM_CACHE_CELLS, ROOM_CACHE_CELL_VERTICES, ROOM_CACHE_SURFACES,
    ROOM_CACHE_VERTICES, ROOM_CHUNKS, ROOM_RESIDENCY, ROOM_SURFACE_CACHES, ROOM_VISIBILITY,
    VISIBILITY_CELLS, VISIBILITY_PVS, VISIBILITY_PVS_BITS, WEAPONS, WEAPON_HITBOXES,
};
#[cfg(feature = "cd-stream-bench")]
use generated::{WORLD_PACK_START_LBA, WORLD_PACK_TOC};

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
const IMAGE_PROP_DEPTH_BIAS: i32 = 256;
const COLLISION_DEBUG_BUTTON: u16 = button::L3;
const COLLISION_DEBUG_SEGMENTS: usize = 8;
const COLLISION_DEBUG_FLOOR_LIFT: i32 = 8;

const CAMERA_Y_OFFSET: i32 = 1100;
const CAMERA_START_RADIUS: i32 = 2400;
const CAMERA_RADIUS_MIN: i32 = 800;
const CAMERA_RADIUS_MAX: i32 = 5200;
const CAMERA_RADIUS_STEP: i32 = 64;
const CAMERA_START_YAW: Angle = Angle::from_q12(220);
const CAMERA_YAW_STEP: Angle = Angle::from_q12(12);
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
const WORLD_BAND: DepthBand = DepthBand::new(0, OT_DEPTH - 1);
const WORLD_DEPTH_RANGE: DepthRange = DepthRange::new(NEAR_Z, FAR_Z);
#[cfg(feature = "world-grid-visible")]
const ROOM_GRID_VISIBILITY_RADIUS: u16 = 32;
const ROOM_GLOBAL_VISIBILITY_RADIUS_SECTORS: i32 = 64;
#[cfg(feature = "world-grid-visible")]
const ROOM_VISIBLE_CELL_SCREEN_MARGIN: i32 = 0;
#[cfg(feature = "world-grid-visible")]
const ROOM_VISIBLE_CELL_CAMERA_MARGIN: i32 = 96;
#[cfg(feature = "world-grid-visible")]
const ROOM_VISIBLE_CELL_SAFETY_RING: i32 = 1;
#[cfg(feature = "world-grid-visible")]
const ROOM_VISIBLE_CELL_NEAR_RING: i32 = 4;
#[cfg(feature = "world-grid-visible")]
const ROOM_VISIBLE_CELL_REAR_RING: i32 = 6;
#[cfg(feature = "world-grid-visible")]
const ROOM_VISIBLE_CELL_WEDGE_MARGIN_SECTORS: i32 = 3;
#[cfg(feature = "world-grid-visible")]
const ROOM_VISIBLE_CELL_WEDGE_NUM: u64 = 3;
#[cfg(feature = "world-grid-visible")]
const ROOM_VISIBLE_CELL_WEDGE_DEN: u64 = 4;
#[cfg(feature = "world-grid-visible")]
const MAX_PRECOMPUTED_VISIBLE_CELLS: usize = 512;
#[cfg(feature = "world-grid-visible")]
const MAX_ACTIVE_VISIBLE_CELLS: usize = 1024;
/// Per-frame projected scratch for one generated room surface cache.
/// Rooms that exceed this vertex budget fall back to the uncached draw.
const MAX_CACHED_ROOM_VERTICES: usize = 4096;

const MAX_TEXTURED_TRIS: usize = 3328;

/// Cap on the per-room material slot count. Picked to comfortably
/// exceed the cooker's currently-emitted material count without
/// over-reserving VRAM or RAM. If a future room exceeds this,
/// the runtime fails graceful (skips the over-cap material) and
/// the cook report should also flag.
const MAX_ROOM_MATERIALS: usize = 32;
/// Current generated chunk plus the best cache-budgeted nearby chunks.
const MAX_ACTIVE_ROOMS: usize = 4;
/// Streamed room slot budget. A slot stores the room `.psxw` plus
/// the room-local render cache records carried by the `.psxc` chunk.
/// Keeping this at 16 sectors forces oversized authored rooms to be
/// split at cook time instead of quietly reserving a larger RAM cache.
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_SLOT_BYTES: usize = 32 * 1024;
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_SLOT_WORDS: usize = STREAMED_ROOM_SLOT_BYTES / 4;
/// CD-backed room residency cache. This is deliberately just larger than the
/// visible active window so chunk-boundary traversal can retain overlap from
/// the previous window without reserving a second full window.
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_SLOT_COUNT: usize = 6;
#[cfg(feature = "cd-stream-bench")]
const MAX_COLLISION_ROOMS: usize = STREAMED_ROOM_SLOT_COUNT;
#[cfg(not(feature = "cd-stream-bench"))]
const MAX_COLLISION_ROOMS: usize = MAX_ACTIVE_ROOMS;
/// Opportunistic lookahead for CD-backed rooms. Prefetch is only allowed when
/// WORLD.PAK metadata proves the extra read span is tiny, so it cannot turn a
/// local active-window read into a broad sector sweep.
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_PREFETCH_COUNT: usize = 1;
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_PREFETCH_MAX_EXTRA_SECTORS: u32 = 8;
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_PREFETCH_MAX_TOTAL_SECTORS: u32 = 48;
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_PUMP_SECTORS_PER_TICK: usize = 8;
#[cfg(feature = "cd-stream-bench")]
const STREAMED_ROOM_BOOTSTRAP_PUMP_LIMIT: usize = 512;
const MAX_SKIPPED_ACTIVE_ROOM_CANDIDATES: usize = 24;
const ACTIVE_ROOM_REFRESH_SECTORS: i32 = 4;
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
static mut CACHED_ROOM_ACCEPTED_CELL_INDICES: [u16; MAX_PRECOMPUTED_VISIBLE_CELLS] =
    [0; MAX_PRECOMPUTED_VISIBLE_CELLS];
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
    visual_offset: [i16; 3],
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
            visual_offset: c.visual_offset,
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
    job: cd_stream::WorldRoomSlotsReadJob<N>,
    job_plan: RoomStreamLoadPlan<N>,
    epoch: u32,
    window_requests: u16,
    window_misses: u16,
    window_prefetch_requests: u16,
    window_evictions: u16,
    window_failed_loads: u16,
    window_pending_loads: u16,
}

#[cfg(feature = "cd-stream-bench")]
impl<const N: usize> RoomStreamScheduler<N> {
    const fn new() -> Self {
        Self {
            slots: [StreamedRoomSlot::EMPTY; N],
            job: cd_stream::WorldRoomSlotsReadJob::new(),
            job_plan: RoomStreamLoadPlan::EMPTY,
            epoch: 0,
            window_requests: 0,
            window_misses: 0,
            window_prefetch_requests: 0,
            window_evictions: 0,
            window_failed_loads: 0,
            window_pending_loads: 0,
        }
    }

    fn begin_window(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.window_requests = 0;
        self.window_misses = 0;
        self.window_prefetch_requests = 0;
        self.window_evictions = 0;
        self.window_failed_loads = 0;
        self.window_pending_loads = 0;
    }

    fn resident_slot_for(&mut self, room: RoomIndex) -> Option<usize> {
        let mut slot = 0usize;
        while slot < N {
            let meta = self.slots[slot];
            if meta.state == RoomStreamSlotState::Resident && meta.room == room {
                self.slots[slot].last_used = self.epoch;
                return Some(slot);
            }
            slot += 1;
        }
        None
    }

    fn is_resident(&self, room: RoomIndex) -> bool {
        let mut slot = 0usize;
        while slot < N {
            let meta = self.slots[slot];
            if meta.state == RoomStreamSlotState::Resident && meta.room == room {
                return true;
            }
            slot += 1;
        }
        false
    }

    fn resident_byte_count(&self, slot: usize) -> Option<usize> {
        let meta = *self.slots.get(slot)?;
        if meta.state == RoomStreamSlotState::Resident && meta.byte_count > 0 {
            Some(meta.byte_count)
        } else {
            None
        }
    }

    fn loading_slot_for(&self, room: RoomIndex) -> Option<usize> {
        let mut slot = 0usize;
        while slot < N {
            let meta = self.slots[slot];
            if meta.state == RoomStreamSlotState::Loading && meta.room == room {
                return Some(slot);
            }
            slot += 1;
        }
        None
    }

    fn plan_window_loads(
        &mut self,
        requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        requested_count: usize,
        active_count: usize,
    ) -> RoomStreamLoadPlan<N> {
        let mut plan = RoomStreamLoadPlan::EMPTY;
        let can_schedule_new_loads = !self.job.is_active();
        let limit = requested_count.min(N).min(STREAMED_ROOM_SLOT_COUNT);
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
            let Some(target) =
                self.choose_slot(requested_rooms, requested_count, &plan.slots, plan.count)
            else {
                i += 1;
                continue;
            };
            if self.slots[target].state == RoomStreamSlotState::Resident {
                self.window_evictions = self.window_evictions.saturating_add(1);
            }
            self.slots[target] = StreamedRoomSlot {
                room,
                byte_count: 0,
                last_used: self.epoch,
                state: RoomStreamSlotState::Loading,
            };
            plan.rooms[plan.count] = room;
            plan.slots[plan.count] = target;
            plan.count += 1;
            self.window_pending_loads = self.window_pending_loads.saturating_add(1);
            i += 1;
        }
        plan
    }

    fn start_load_plan(&mut self, plan: RoomStreamLoadPlan<N>) {
        if plan.count == 0 || self.job.is_active() {
            return;
        }
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
                self.slots[target] = StreamedRoomSlot {
                    room: plan.rooms[i],
                    byte_count: byte_counts[i],
                    last_used: self.epoch,
                    state: RoomStreamSlotState::Resident,
                };
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
                self.slots[target] = StreamedRoomSlot {
                    room: plan.rooms[loaded],
                    byte_count: byte_counts[loaded],
                    last_used: self.epoch,
                    state: RoomStreamSlotState::Resident,
                };
            } else {
                self.slots[target] = StreamedRoomSlot {
                    room: plan.rooms[loaded],
                    byte_count: 0,
                    last_used: self.epoch,
                    state: RoomStreamSlotState::Failed,
                };
                self.window_failed_loads = self.window_failed_loads.saturating_add(1);
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
    }

    fn resident_slot_count(&self) -> usize {
        let mut count = 0usize;
        let mut slot = 0usize;
        while slot < N {
            if self.slots[slot].state == RoomStreamSlotState::Resident {
                count += 1;
            }
            slot += 1;
        }
        count
    }

    fn choose_slot(
        &self,
        requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        requested_count: usize,
        reserved_slots: &[usize; N],
        reserved_count: usize,
    ) -> Option<usize> {
        let mut slot = 0usize;
        while slot < N {
            let state = self.slots[slot].state;
            if (state == RoomStreamSlotState::Empty || state == RoomStreamSlotState::Failed)
                && !streamed_slot_reserved(slot, reserved_slots, reserved_count)
            {
                return Some(slot);
            }
            slot += 1;
        }

        let mut best_slot = None;
        let mut best_age = u32::MAX;
        let mut candidate = 0usize;
        while candidate < N {
            let meta = self.slots[candidate];
            if streamed_slot_reserved(candidate, reserved_slots, reserved_count)
                || room_requested(meta.room, requested_rooms, requested_count)
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
}

#[cfg(feature = "world-grid-visible")]
#[derive(Copy, Clone)]
struct ActiveVisibleCellCache {
    room: RoomIndex,
    anchor_x: i32,
    anchor_z: i32,
    view_sin_key: i16,
    view_cos_key: i16,
    rejected_global: u16,
    first: u16,
    count: u16,
    ready: bool,
}

#[cfg(feature = "world-grid-visible")]
impl ActiveVisibleCellCache {
    const EMPTY: Self = Self {
        room: RoomIndex::ZERO,
        anchor_x: 0,
        anchor_z: 0,
        view_sin_key: 0,
        view_cos_key: 0,
        rejected_global: 0,
        first: 0,
        count: 0,
        ready: false,
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
    #[cfg(feature = "world-grid-visible")]
    visible_cell_caches: [ActiveVisibleCellCache; MAX_ACTIVE_ROOMS],
    #[cfg(feature = "world-grid-visible")]
    visible_cell_cache_cells: [GridVisibleCell; MAX_ACTIVE_VISIBLE_CELLS],
    #[cfg(feature = "world-grid-visible")]
    visible_cell_cache_cursor: usize,
    active_room_candidates: u16,
    active_room_cache_skips: u16,
    active_room_anchor: RoomPoint,
    active_room_view_sin_key: i16,
    active_room_view_cos_key: i16,
    /// Index in ROOMS the player is currently in. Used to scope
    /// model-instance + light queries.
    room_index: RoomIndex,
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
    anim_start_tick: u32,
    /// Non-looping gameplay animation lock. While active,
    /// locomotion input is ignored and the current action clip
    /// plays from start to finish.
    anim_lock_until_tick: u32,
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
    /// Immediate-mode cylinder overlay for tuning actor blockers.
    show_collision_debug: bool,
}

impl Playtest {
    const fn new() -> Self {
        Self {
            room: None,
            current_collision_room: None,
            current_ambient_rgb: [0x80, 0x80, 0x80],
            active_rooms: [const { None }; MAX_ACTIVE_ROOMS],
            #[cfg(feature = "world-grid-visible")]
            visible_cell_caches: [const { ActiveVisibleCellCache::EMPTY }; MAX_ACTIVE_ROOMS],
            #[cfg(feature = "world-grid-visible")]
            visible_cell_cache_cells: [GridVisibleCell::EMPTY; MAX_ACTIVE_VISIBLE_CELLS],
            #[cfg(feature = "world-grid-visible")]
            visible_cell_cache_cursor: 0,
            active_room_candidates: 0,
            active_room_cache_skips: 0,
            active_room_anchor: RoomPoint::ZERO,
            active_room_view_sin_key: 0,
            active_room_view_cos_key: 0,
            room_index: RoomIndex::ZERO,
            materials: [room_material_fallback(); MAX_ROOM_MATERIALS],
            material_count: 0,
            motor: CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO),
            character: None,
            anim_state: PlayerAnim::Idle,
            anim_start_tick: 0,
            anim_lock_until_tick: 0,
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
            show_collision_debug: false,
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

        // Empty manifest? Boot to a clear-coloured screen.
        if ROOMS.is_empty() {
            return;
        };

        // Player init: prefer PLAYER_CONTROLLER (cook output)
        // for spawn + character; fall back to the bare
        // PLAYER_SPAWN for placeholder manifests. The spawn room
        // may be a generated chunk rather than room zero.
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
        self.anim_start_tick = 0;
        self.camera.snap_to_player_with_yaw(
            self.camera_target(None, false),
            self.camera_config(),
            CAMERA_START_YAW,
        );
        self.load_active_room_window();
        #[cfg(feature = "cd-stream-bench")]
        self.bootstrap_streamed_room_window();
        #[cfg(feature = "cd-stream-benchmark")]
        cd_stream::run_benchmark();
    }

    fn update(&mut self, ctx: &mut Ctx) {
        #[cfg(feature = "cd-stream-bench")]
        if self.pump_room_stream(STREAMED_ROOM_PUMP_SECTORS_PER_TICK) {
            self.load_active_room_window();
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
            return;
        }

        if ctx.just_pressed(button::SELECT) {
            self.free_orbit = !self.free_orbit;
        }
        let delta_vblanks = ctx.time.delta_vblanks();
        if self.free_orbit {
            let (right_x, right_y) = ctx.pad.sticks.right_centered();
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
            self.refresh_active_room_window_if_needed();
            telemetry::stage_begin(telemetry::stage::CAMERA);
            self.render_camera = self.free_orbit_camera();
            telemetry::stage_end(telemetry::stage::CAMERA);
            return;
        }

        let now = ctx.time.elapsed_vblanks();
        let action_locked = self.anim_lock_until_tick > now;
        let circle = self.update_evade_run_button(ctx, delta_vblanks);
        let mut input = if action_locked {
            CharacterMotorInput::default()
        } else {
            motor_input(ctx, self.camera.yaw(), circle.sprint, circle.evade)
        };
        if !action_locked && self.motor.action().is_idle() {
            let started = if ctx.just_pressed(LIGHT_ATTACK_BUTTON) {
                self.start_player_anim_action(PlayerAnim::LightAttack, now, ctx.time.video_hz())
            } else if ctx.just_pressed(HEAVY_ATTACK_BUTTON) {
                self.start_player_anim_action(PlayerAnim::HeavyAttack, now, ctx.time.video_hz())
            } else {
                false
            };
            if started {
                input = CharacterMotorInput::default();
            }
        }
        let config = self.motor_config();
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
        let collision = if collision_room_count <= 1 {
            CharacterCollision::new(room_collision, &blockers[..blocker_count])
        } else {
            CharacterCollision::rooms(
                &collision_rooms[..collision_room_count],
                &blockers[..blocker_count],
            )
        };
        let motor_frame =
            self.motor
                .update_vblanks_with_collision(collision, input, config, delta_vblanks);
        if !self.update_current_room_from_player() {
            self.refresh_active_room_window_if_needed();
        }

        let new_state = if self.anim_lock_until_tick > now {
            self.anim_state
        } else {
            player_anim_from_motor(motor_frame.anim)
        };
        if new_state != self.anim_state {
            self.anim_state = new_state;
            self.anim_start_tick = now;
        }

        if self.lock_target.is_some() {
            if !self.lock_target_valid(LOCK_BREAK_RANGE) {
                self.lock_target = None;
                self.lock_switch_stick_held = false;
            } else {
                self.update_lock_target_switch(ctx);
            }
        }
        if SOFT_LOCK_ENABLED {
            self.update_soft_lock(ctx);
        } else {
            self.soft_lock_target = None;
            self.soft_lock_suppressed = false;
        }

        telemetry::stage_begin(telemetry::stage::CAMERA);
        self.render_camera = self.update_follow_camera(ctx);
        telemetry::stage_end(telemetry::stage::CAMERA);
    }

    fn render(&mut self, ctx: &mut Ctx) {
        if !ctx.pad.is_analog() {
            if let Some(font) = self.font.as_ref() {
                draw_analog_required_prompt(font);
            }
            return;
        }

        let camera = self.render_camera;

        let mut ot = unsafe { OtFrame::begin(&mut OT) };
        let mut primitive_packets = unsafe { PrimitivePacketArena::new(&mut PRIMITIVE_PACKETS) };
        let mut world = unsafe { begin_world_render_pass(&mut ot, &mut WORLD_COMMANDS) };

        if let Some(room_record) = ROOMS.get(self.room_index.to_usize()) {
            telemetry::stage_begin(telemetry::stage::SKY);
            draw_sky_panorama(room_record.sky, camera);
            telemetry::stage_end(telemetry::stage::SKY);
            telemetry::stage_begin(telemetry::stage::FAR_VISTA);
            draw_far_vista_ring(
                camera,
                room_record.far_vista,
                &mut primitive_packets,
                &mut world,
            );
            telemetry::stage_end(telemetry::stage::FAR_VISTA);
        }

        if self.current_collision_room.is_some() {
            let room_options = WorldSurfaceOptions::new(WORLD_BAND, WORLD_DEPTH_RANGE);
            let actor_options = WorldSurfaceOptions::new(WORLD_BAND, WORLD_DEPTH_RANGE);
            let mut total_instance_stats = ModelInstanceDrawStats::default();
            let mut room_active_chunks = 0u32;
            let mut room_cached_draws = 0u32;
            let mut room_uncached_draws = 0u32;
            let mut room_cache_cells = 0u32;
            let mut room_cache_vertices = 0u32;
            let mut room_cache_surfaces = 0u32;
            let mut room_cache_fallback_draws = 0u32;
            let mut room_visibility_fallback_draws = 0u32;
            #[cfg(feature = "world-grid-visible")]
            let mut room_visible_cells = 0u32;
            #[cfg(feature = "world-grid-visible")]
            let mut room_range_culled_cells = 0u32;
            #[cfg(feature = "world-grid-visible")]
            let mut room_stats_total = GridVisibilityStats::default();

            let active_draw_order = active_room_draw_order(&self.active_rooms, camera);
            for &active_slot in &active_draw_order {
                if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                    continue;
                }
                let active_slot = active_slot as usize;
                let Some(active) = self.active_rooms[active_slot] else {
                    continue;
                };
                room_active_chunks = room_active_chunks.saturating_add(1);
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
                    let player = self.motor.position();
                    let global_visibility_anchor =
                        RoomPoint::new(camera.position.x, player.y, camera.position.z);
                    let visibility_anchor = RoomPoint::new(
                        global_visibility_anchor.x.saturating_sub(active.offset_x),
                        player.y,
                        global_visibility_anchor.z.saturating_sub(active.offset_z),
                    );
                    let visibility =
                        GridVisibility::around(visibility_anchor, ROOM_GRID_VISIBILITY_RADIUS)
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
                    );
                    telemetry::stage_end(telemetry::stage::ROOM_VISIBLE_LIST);
                    let stats = if let Some((cells, range_culled)) = visible_cells_result {
                        room_range_culled_cells =
                            room_range_culled_cells.saturating_add(range_culled as u32);
                        room_visible_cells = room_visible_cells.saturating_add(cells.len() as u32);
                        if active.surface_cache.ready {
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
                    accumulate_grid_visibility_stats(&mut room_stats_total, stats);
                }
                #[cfg(not(feature = "world-grid-visible"))]
                {
                    room_uncached_draws = room_uncached_draws.saturating_add(1);
                    if active_surface_cache_failed(active.surface_cache) {
                        room_cache_fallback_draws = room_cache_fallback_draws.saturating_add(1);
                    }
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
                        shadow_material,
                        &self.models,
                        &mut primitive_packets,
                        &mut world,
                    );
                }
                let instance_stats = draw_model_instances(
                    active.index,
                    ctx.time.elapsed_vblanks(),
                    ctx.time.video_hz(),
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
                telemetry::stage_begin(telemetry::stage::PLAYER);
                if let Some(shadow_material) = self.shadow_material {
                    draw_actor_shadow(
                        player.x,
                        player.y,
                        player.z,
                        actor_shadow_radius(character.radius),
                        &camera,
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
                            character.clip_for(self.anim_state),
                            self.anim_start_tick,
                            ctx.time.elapsed_vblanks(),
                            ctx.time.video_hz(),
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
                            character.clip_for(self.anim_state),
                            self.anim_start_tick,
                            ctx.time.elapsed_vblanks(),
                            ctx.time.video_hz(),
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
                        ctx.time.elapsed_vblanks(),
                        ctx.time.video_hz(),
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
        }

        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVES,
            primitive_packets.len() as u32,
        );
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVE_REMAINING,
            primitive_packets.remaining() as u32,
        );
        telemetry::counter(
            telemetry::counter::WORLD_COMMANDS,
            world.command_len() as u32,
        );
        telemetry::stage_begin(telemetry::stage::WORLD_FLUSH);
        world.flush();
        telemetry::stage_end(telemetry::stage::WORLD_FLUSH);
        telemetry::stage_begin(telemetry::stage::OT_SUBMIT);
        ot.submit();
        telemetry::stage_end(telemetry::stage::OT_SUBMIT);

        if self.show_collision_debug {
            self.draw_collision_debug_overlay(camera);
        }

        if let Some(target) = self.lock_target_indicator_position() {
            draw_lock_target_indicator(target, camera, ctx.time.elapsed_vblanks());
        }

        if self.character.is_some() {
            draw_player_hud(
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
    fn start_player_anim_action(&mut self, anim: PlayerAnim, now: u32, video_hz: u16) -> bool {
        let Some(character) = self.character else {
            return false;
        };
        if character.action_clip(anim.action()).is_none() {
            return false;
        }
        let clip = character.clip_for(anim);
        let duration = self
            .player_clip_duration_vblanks(character, clip, video_hz)
            .unwrap_or(24)
            .max(1);
        self.anim_state = anim;
        self.anim_start_tick = now;
        self.anim_lock_until_tick = now.saturating_add(duration);
        true
    }

    fn player_clip_duration_vblanks(
        &self,
        character: RuntimeCharacter,
        clip: ModelClipIndex,
        video_hz: u16,
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
                .saturating_mul(video_hz.max(1) as u32)
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
            count += 1;
        }
        #[cfg(feature = "cd-stream-bench")]
        {
            count = self.collect_resident_streamed_collision_rooms(
                current_authored,
                anchor,
                margin,
                out,
                count,
            );
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
        mut count: usize,
    ) -> usize {
        let Some(current_record) = ROOMS.get(self.room_index.to_usize()) else {
            return count;
        };
        for chunk in ROOM_CHUNKS {
            if count >= out.len() {
                break;
            }
            if chunk.room == self.room_index
                || active_room_window_contains(&self.active_rooms, chunk.room)
            {
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
            self.spawn.to_world_vertex(),
            CAMERA_Y_OFFSET,
            self.orbit_radius,
            self.orbit_yaw,
        )
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
                    ctx.time.delta_vblanks(),
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
            .update_vblanks(
                PROJECTION,
                collision,
                target,
                input,
                config,
                ctx.time.delta_vblanks(),
            )
            .camera
    }

    fn chunked_level(&self) -> bool {
        self.active_rooms
            .iter()
            .flatten()
            .any(|room| room.index != self.room_index)
    }

    fn active_room_selection_view(&self) -> ActiveRoomView {
        if self.free_orbit {
            let yaw = self.orbit_yaw;
            let position = RoomPoint::new(
                self.spawn
                    .x
                    .saturating_add(yaw.sin().mul_i32(self.orbit_radius)),
                self.spawn.y,
                self.spawn
                    .z
                    .saturating_add(yaw.cos().mul_i32(self.orbit_radius)),
            );
            ActiveRoomView::new(position, yaw)
        } else {
            ActiveRoomView::new(self.camera.position(), self.camera.yaw())
        }
    }

    fn load_active_room_window(&mut self) {
        telemetry::stage_begin(telemetry::stage::ACTIVE_ROOM_WINDOW);
        telemetry::counter(telemetry::counter::ROOM_WINDOW_REBUILDS, 1);
        self.room = None;
        self.current_collision_room = None;
        self.current_ambient_rgb = [0x80, 0x80, 0x80];
        self.materials = [room_material_fallback(); MAX_ROOM_MATERIALS];
        self.material_count = 0;
        self.active_rooms = [const { None }; MAX_ACTIVE_ROOMS];
        self.active_room_candidates = 0;
        self.active_room_cache_skips = 0;
        #[cfg(feature = "world-grid-visible")]
        {
            self.clear_visible_cell_caches();
        }

        let current_index = self.room_index;
        let Some(current_record) = ROOMS.get(current_index.to_usize()) else {
            telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);
            return;
        };
        #[cfg(not(feature = "cd-stream-bench"))]
        let current_room = parse_runtime_room(current_record);
        #[cfg(feature = "cd-stream-bench")]
        let current_room: Option<RuntimeRoom<'static>> = None;
        #[cfg(not(feature = "cd-stream-bench"))]
        let Some(current_room) = current_room
        else {
            telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);
            return;
        };
        #[cfg(feature = "cd-stream-bench")]
        if current_room.is_none() && ROOM_CHUNKS.is_empty() {
            telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);
            return;
        }
        let player = self.motor.position();
        let view = self.active_room_selection_view();
        self.active_room_view_sin_key = view.sin_key;
        self.active_room_view_cos_key = view.cos_key;

        let mut next_slot = 0usize;
        if let Some(active) =
            build_active_room(next_slot, current_index, current_record, current_record)
        {
            self.room = active.render_room;
            self.current_collision_room = Some(active.collision_room);
            self.current_ambient_rgb = active.ambient_rgb;
            self.materials = active.materials;
            self.material_count = active.material_count;
            self.active_rooms[next_slot] = Some(active);
            next_slot += 1;
        }
        self.active_room_anchor = player;
        let mut skipped_rooms = [INVALID_ROOM_INDEX; MAX_SKIPPED_ACTIVE_ROOM_CANDIDATES];
        let mut skipped_count = 0usize;

        if !ROOM_CHUNKS.is_empty() {
            self.active_room_candidates =
                count_spatial_chunk_candidates(current_index, current_record, player, view);
            while next_slot < MAX_ACTIVE_ROOMS {
                let Some(candidate) = best_spatial_chunk_candidate(
                    current_index,
                    current_record,
                    player,
                    view,
                    &self.active_rooms,
                    &skipped_rooms[..skipped_count],
                ) else {
                    break;
                };
                let Some(record) = ROOMS.get(candidate.to_usize()) else {
                    break;
                };
                if let Some(active) =
                    build_active_room(next_slot, candidate, record, current_record)
                {
                    if active.surface_cache.ready {
                        self.active_rooms[next_slot] = Some(active);
                        next_slot += 1;
                        continue;
                    }
                };
                self.active_room_cache_skips = self.active_room_cache_skips.saturating_add(1);
                if skipped_count >= skipped_rooms.len() {
                    break;
                }
                skipped_rooms[skipped_count] = candidate;
                skipped_count += 1;
            }
        } else {
            #[cfg(feature = "cd-stream-bench")]
            let Some(current_room) = current_room
            else {
                telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);
                return;
            };
            while next_slot < MAX_ACTIVE_ROOMS {
                let Some(raw_index) = nearest_touching_room_index(
                    current_index,
                    current_record,
                    current_room,
                    &self.active_rooms,
                ) else {
                    break;
                };
                let Some(record) = ROOMS.get(raw_index) else {
                    break;
                };
                let index = RoomIndex::new(raw_index as u16);
                if let Some(active) = build_active_room(next_slot, index, record, current_record) {
                    self.active_rooms[next_slot] = Some(active);
                    next_slot += 1;
                } else {
                    break;
                }
            }
        }
        telemetry::counter(
            telemetry::counter::ROOM_WINDOW_BUILT_CHUNKS,
            next_slot as u32,
        );
        #[cfg(feature = "cd-stream-bench")]
        self.preload_streamed_active_room_window(
            next_slot,
            current_record,
            &skipped_rooms,
            skipped_count,
        );
        telemetry::stage_end(telemetry::stage::ACTIVE_ROOM_WINDOW);
    }

    #[cfg(feature = "cd-stream-bench")]
    fn preload_streamed_active_room_window(
        &mut self,
        active_count: usize,
        current_record: &LevelRoomRecord,
        skipped_rooms: &[RoomIndex; MAX_SKIPPED_ACTIVE_ROOM_CANDIDATES],
        skipped_count: usize,
    ) {
        let active_count = active_count.min(MAX_ACTIVE_ROOMS);
        let mut room_ids = [u16::MAX; MAX_ACTIVE_ROOMS];
        let mut requested_rooms = [INVALID_ROOM_INDEX; STREAMED_ROOM_SLOT_COUNT];
        let mut requested_count = 0usize;
        let mut i = 0usize;
        while i < active_count {
            let Some(active) = self.active_rooms[i] else {
                return;
            };
            room_ids[i] = active.index.raw();
            requested_rooms[requested_count] = active.index;
            requested_count += 1;
            i += 1;
        }
        if requested_count == 0 {
            requested_rooms[requested_count] = self.room_index;
            requested_count += 1;
        }
        requested_count =
            self.append_current_collision_neighbour_requests(&mut requested_rooms, requested_count);
        let mut skipped = 0usize;
        while skipped < skipped_count
            && requested_count < STREAMED_ROOM_SLOT_COUNT
            && skipped < skipped_rooms.len()
        {
            let room = skipped_rooms[skipped];
            if room != INVALID_ROOM_INDEX
                && !room_requested(room, &requested_rooms, requested_count)
                && streamed_room_chunk_loadable(room)
            {
                requested_rooms[requested_count] = room;
                requested_count += 1;
            }
            skipped += 1;
        }

        let priority_count = requested_count;
        let requested_count =
            self.append_pack_aware_streamed_prefetches(&mut requested_rooms, requested_count);
        self.ensure_streamed_room_residency(&requested_rooms, requested_count, priority_count);

        let mut rebuilt = [const { None }; MAX_ACTIVE_ROOMS];
        let mut slot = 0usize;
        while slot < active_count {
            let index = RoomIndex::new(room_ids[slot]);
            if let Some(record) = ROOMS.get(index.to_usize()) {
                rebuilt[slot] = build_active_room(slot, index, record, current_record);
            }
            slot += 1;
        }
        self.active_rooms = rebuilt;
        if let Some(active) = self.active_rooms[0] {
            self.room = active.render_room;
            self.current_collision_room = Some(active.collision_room);
            self.current_ambient_rgb = active.ambient_rgb;
            self.materials = active.materials;
            self.material_count = active.material_count;
        }
    }

    #[cfg(feature = "cd-stream-bench")]
    fn append_current_collision_neighbour_requests(
        &self,
        requested_rooms: &mut [RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        requested_count: usize,
    ) -> usize {
        let Some(chunk) = chunk_record_for_room(self.room_index) else {
            return requested_count.min(STREAMED_ROOM_SLOT_COUNT);
        };
        let mut count = requested_count.min(STREAMED_ROOM_SLOT_COUNT);
        for room in [
            chunk.neighbours.north,
            chunk.neighbours.east,
            chunk.neighbours.south,
            chunk.neighbours.west,
        ] {
            count = append_streamed_room_request(requested_rooms, count, room);
        }
        count
    }

    #[cfg(feature = "cd-stream-bench")]
    fn append_pack_aware_streamed_prefetches(
        &self,
        requested_rooms: &mut [RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        active_count: usize,
    ) -> usize {
        let mut requested_count = active_count.min(STREAMED_ROOM_SLOT_COUNT);
        if requested_count == 0
            || requested_count >= STREAMED_ROOM_SLOT_COUNT
            || STREAMED_ROOM_PREFETCH_COUNT == 0
            || ROOM_CHUNKS.is_empty()
        {
            return requested_count;
        }

        let Some(current_record) = ROOMS.get(self.room_index.to_usize()) else {
            return requested_count;
        };
        let player = self.motor.position();
        let view = self.active_room_selection_view();

        let mut added = 0usize;
        while added < STREAMED_ROOM_PREFETCH_COUNT && requested_count < STREAMED_ROOM_SLOT_COUNT {
            let Some((span_start, span_end)) =
                streamed_request_sector_span(requested_rooms, requested_count)
            else {
                break;
            };
            let base_span = span_end.saturating_sub(span_start);
            let mut best_room = INVALID_ROOM_INDEX;
            let mut best_score = ChunkActivationScore::WORST;
            let mut best_extra_sectors = u32::MAX;
            let mut best_total_sectors = u32::MAX;

            for chunk in ROOM_CHUNKS {
                if chunk.room == self.room_index
                    || room_requested(chunk.room, requested_rooms, requested_count)
                    || streamed_room_is_resident(chunk.room)
                {
                    continue;
                }
                let Some(score) =
                    chunk_activation_score(*chunk, self.room_index, current_record, player, view)
                else {
                    continue;
                };
                let Some(info) = cd_stream::world_room_chunk_info(chunk.room.raw(), WORLD_PACK_TOC)
                else {
                    continue;
                };
                if info.sector_count == 0
                    || info.byte_size == 0
                    || info.byte_size > STREAMED_ROOM_SLOT_BYTES
                {
                    continue;
                }
                let candidate_end = info.sector_offset.saturating_add(info.sector_count);
                let total_sectors = span_end
                    .max(candidate_end)
                    .saturating_sub(span_start.min(info.sector_offset));
                let extra_sectors = total_sectors.saturating_sub(base_span);
                if extra_sectors > STREAMED_ROOM_PREFETCH_MAX_EXTRA_SECTORS
                    || (extra_sectors > 0
                        && total_sectors > STREAMED_ROOM_PREFETCH_MAX_TOTAL_SECTORS)
                {
                    continue;
                }
                let better = best_room == INVALID_ROOM_INDEX
                    || extra_sectors < best_extra_sectors
                    || (extra_sectors == best_extra_sectors
                        && (score.better_than(best_score)
                            || (best_total_sectors > total_sectors
                                && !best_score.better_than(score))));
                if better {
                    best_room = chunk.room;
                    best_score = score;
                    best_extra_sectors = extra_sectors;
                    best_total_sectors = total_sectors;
                }
            }

            if best_room == INVALID_ROOM_INDEX {
                break;
            }
            requested_rooms[requested_count] = best_room;
            requested_count += 1;
            added += 1;
        }

        requested_count
    }

    #[cfg(feature = "cd-stream-bench")]
    fn ensure_streamed_room_residency(
        &mut self,
        requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
        requested_count: usize,
        active_count: usize,
    ) {
        let plan = unsafe {
            ROOM_STREAM_SCHEDULER.begin_window();
            ROOM_STREAM_SCHEDULER.plan_window_loads(
                requested_rooms,
                requested_count,
                active_count.min(requested_count),
            )
        };
        if plan.count == 0 {
            unsafe {
                ROOM_STREAM_SCHEDULER.emit_counters();
            }
            return;
        }

        unsafe {
            ROOM_STREAM_SCHEDULER.start_load_plan(plan);
            ROOM_STREAM_SCHEDULER.emit_counters();
        }
    }

    #[cfg(feature = "cd-stream-bench")]
    fn pump_room_stream(&mut self, max_sectors: usize) -> bool {
        unsafe { ROOM_STREAM_SCHEDULER.pump(&mut STREAMED_ROOM_WORDS, max_sectors) }
    }

    #[cfg(feature = "cd-stream-bench")]
    fn bootstrap_streamed_room_window(&mut self) {
        let mut pumps = 0usize;
        while pumps < STREAMED_ROOM_BOOTSTRAP_PUMP_LIMIT && streamed_room_stream_active() {
            if self.pump_room_stream(STREAMED_ROOM_PUMP_SECTORS_PER_TICK) {
                self.load_active_room_window();
            }
            pumps += 1;
        }
        if self.current_collision_room.is_none() {
            self.load_active_room_window();
        }
    }

    fn update_current_room_from_player(&mut self) -> bool {
        if !self.chunked_level() {
            return false;
        }
        let global = local_to_global_room_point(self.room_index, self.motor.position());
        let Some(next_room) = room_index_containing_global(global) else {
            return false;
        };
        if next_room == self.room_index {
            return false;
        }
        let previous_local = self.motor.position();
        let local = global_to_local_room_point(next_room, global);
        let camera_delta = RoomPoint::new(
            local.x.saturating_sub(previous_local.x),
            local.y.saturating_sub(previous_local.y),
            local.z.saturating_sub(previous_local.z),
        );
        self.room_index = next_room;
        self.motor.relocate(local);
        self.camera.relocate_room_space(camera_delta);
        self.lock_target = None;
        self.lock_switch_stick_held = false;
        self.soft_lock_target = None;
        self.load_active_room_window();
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
        let threshold = sector_size.saturating_mul(ACTIVE_ROOM_REFRESH_SECTORS.max(1));
        let player = self.motor.position();
        let view = self.active_room_selection_view();
        let view_changed = self.active_room_view_sin_key != view.sin_key
            || self.active_room_view_cos_key != view.cos_key;
        if !view_changed
            && point_xz_distance_sq(player, self.active_room_anchor)
                < (threshold as u64).saturating_mul(threshold as u64)
        {
            return;
        }
        if self.active_room_window_matches_selection(self.room_index, record, player, view) {
            self.active_room_anchor = player;
            self.active_room_view_sin_key = view.sin_key;
            self.active_room_view_cos_key = view.cos_key;
        } else {
            self.load_active_room_window();
        }
    }

    fn active_room_window_matches_selection(
        &self,
        current_index: RoomIndex,
        current_record: &LevelRoomRecord,
        player: RoomPoint,
        view: ActiveRoomView,
    ) -> bool {
        if ROOM_CHUNKS.is_empty() || self.active_rooms[0].is_none() {
            return false;
        }

        let mut selected = [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS];
        selected[0] = current_index;
        let mut selected_count = 1usize;
        while selected_count < MAX_ACTIVE_ROOMS {
            let Some(candidate) = best_spatial_chunk_candidate_from_indices(
                current_index,
                current_record,
                player,
                view,
                &selected[..selected_count],
            ) else {
                break;
            };
            selected[selected_count] = candidate;
            selected_count += 1;
        }

        let mut slot = 0usize;
        while slot < MAX_ACTIVE_ROOMS {
            match self.active_rooms[slot] {
                Some(active) if slot < selected_count && active.index == selected[slot] => {}
                None if slot >= selected_count => {}
                _ => return false,
            }
            slot += 1;
        }
        true
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
    clip_local: ModelClipIndex,
    anim_start_tick: u32,
    elapsed_vblanks: u32,
    video_hz: u16,
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
    let local_tick = elapsed_vblanks.saturating_sub(anim_start_tick);
    let phase = anim.phase_at_tick_q12(local_tick, video_hz);

    let instance_rotation = yaw_rotation_matrix(yaw);
    let origin = visual_model_origin(
        x,
        y,
        z,
        runtime_model.world_height,
        character.visual_offset,
        character.visual_scale_q8,
        &instance_rotation,
    );
    let local_to_world = visual_model_local_to_world(runtime_model, character.visual_scale_q8);
    telemetry::stage_begin(telemetry::stage::PLAYER_BOUNDS);
    let visible = match model_frame_bounds(runtime_model, clip_local, phase) {
        Some(bounds) if MODEL_BOUNDS_CULLING_ENABLED => model_bounds_visible(
            camera,
            options,
            origin,
            instance_rotation,
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
        instance_rotation,
        local_to_world,
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
    clip_local: ModelClipIndex,
    anim_start_tick: u32,
    elapsed_vblanks: u32,
    video_hz: u16,
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
    let local_tick = elapsed_vblanks.saturating_sub(anim_start_tick);
    let character_phase = character_anim.phase_at_tick_q12(local_tick, video_hz);
    let character_frame = (character_phase >> 12) as u16;
    let character_rotation = yaw_rotation_matrix(yaw);
    let character_origin = visual_model_origin(
        x,
        y,
        z,
        character_model.world_height,
        character.visual_offset,
        character.visual_scale_q8,
        &character_rotation,
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
            character_rotation,
            character_local_to_world,
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
                    let phase = anim.phase_at_tick_q12(elapsed_vblanks, video_hz);
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
    socket: &LevelModelSocketRecord,
) -> Option<AttachmentPose> {
    let pose = animation.pose_looped_q12(phase_q12, socket.joint)?;
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
fn draw_sky_panorama(sky: LevelSkyRecord, camera: WorldCamera) {
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
    let rows = SKY_PANORAMA_PALETTE_BANDS;
    let horizon_pitch = sky_horizon_pitch_degrees_i32(sky.horizon_percent);
    let top_pitch = (horizon_pitch + 58).min(78);
    let bottom_pitch = (horizon_pitch - 46).max(-72);

    for row in 0..rows {
        let pitch_top = sky_lerp_i32(top_pitch, bottom_pitch, row, rows);
        let pitch_bottom = sky_lerp_i32(top_pitch, bottom_pitch, row + 1, rows);
        let v0 = sky_uv_for_step(row, rows, SKY_PANORAMA_HEIGHT);
        let v1 = sky_uv_for_step(row + 1, rows, SKY_PANORAMA_HEIGHT);
        let clut_word = sky_panorama_clut_word(row);
        for column in 0..columns {
            let yaw0 = sky_yaw_degrees_for_column(column, columns);
            let yaw1 = sky_yaw_degrees_for_column(column + 1, columns);
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
            draw_sky_cyclorama_quad(
                camera,
                material,
                [
                    sky_direction_q12(yaw0, pitch_top),
                    sky_direction_q12(yaw1, pitch_top),
                    sky_direction_q12(yaw0, pitch_bottom),
                    sky_direction_q12(yaw1, pitch_bottom),
                ],
                [(u0, v0), (u1, v0), (u0, v1), (u1, v1)],
            );
        }
    }
}

fn draw_sky_cyclorama_quad(
    camera: WorldCamera,
    material: TextureMaterial,
    dirs: [[i16; 3]; 4],
    uvs: [(u8, u8); 4],
) {
    let Some(projected) = project_sky_quad(dirs, camera) else {
        return;
    };
    if sky_quad_outside_screen(projected) {
        return;
    }
    draw_quad_textured_material(projected, uvs, material);
}

fn project_sky_quad(dirs: [[i16; 3]; 4], camera: WorldCamera) -> Option<[(i16, i16); 4]> {
    Some([
        project_sky_direction(dirs[0], camera)?,
        project_sky_direction(dirs[1], camera)?,
        project_sky_direction(dirs[2], camera)?,
        project_sky_direction(dirs[3], camera)?,
    ])
}

fn project_sky_direction(dir: [i16; 3], camera: WorldCamera) -> Option<(i16, i16)> {
    let x = i32::from(dir[0]);
    let y = i32::from(dir[1]);
    let z = i32::from(dir[2]);
    let sin_yaw = camera.sin_yaw.raw();
    let cos_yaw = camera.cos_yaw.raw();
    let sin_pitch = camera.sin_pitch.raw();
    let cos_pitch = camera.cos_pitch.raw();
    let x1 = mul_q12_i32(x, cos_yaw) - mul_q12_i32(z, sin_yaw);
    let z1 = -mul_q12_i32(x, sin_yaw) - mul_q12_i32(z, cos_yaw);
    let y2 = mul_q12_i32(y, cos_pitch) - mul_q12_i32(z1, sin_pitch);
    let z2 = mul_q12_i32(y, sin_pitch) + mul_q12_i32(z1, cos_pitch);
    if z2 <= NEAR_Z {
        return None;
    }
    let sx = SCREEN_CX as i32 + (x1 * FOCAL) / z2;
    let sy = SCREEN_CY as i32 - (y2 * FOCAL) / z2;
    Some((
        clamp_i16(sx.clamp(-512, 831)),
        clamp_i16(sy.clamp(-256, 495)),
    ))
}

fn sky_quad_outside_screen(points: [(i16, i16); 4]) -> bool {
    let min_x = points.iter().map(|p| p.0).min().unwrap_or(0);
    let max_x = points.iter().map(|p| p.0).max().unwrap_or(0);
    let min_y = points.iter().map(|p| p.1).min().unwrap_or(0);
    let max_y = points.iter().map(|p| p.1).max().unwrap_or(0);
    max_x < 0 || min_x >= SCREEN_W || max_y < 0 || min_y >= SCREEN_H
}

fn sky_direction_q12(yaw_degrees: i32, pitch_degrees: i32) -> [i16; 3] {
    let yaw = angle_from_degrees_i32(yaw_degrees);
    let pitch = angle_from_degrees_i32(pitch_degrees.clamp(-82, 82));
    let yaw_sin = yaw.sin().raw();
    let yaw_cos = yaw.cos().raw();
    let pitch_sin = pitch.sin().raw();
    let pitch_cos = pitch.cos().raw();
    [
        clamp_i16(-mul_q12_i32(yaw_sin, pitch_cos)),
        clamp_i16(pitch_sin),
        clamp_i16(-mul_q12_i32(yaw_cos, pitch_cos)),
    ]
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
            let options = WorldSurfaceOptions::new(WORLD_BAND, WORLD_DEPTH_RANGE)
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
fn streamed_room_chunk_loadable(room: RoomIndex) -> bool {
    cd_stream::world_room_chunk_info(room.raw(), WORLD_PACK_TOC).is_some_and(|info| {
        info.sector_count > 0 && info.byte_size > 0 && info.byte_size <= STREAMED_ROOM_SLOT_BYTES
    })
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

#[cfg(feature = "cd-stream-bench")]
fn append_streamed_room_request(
    requested_rooms: &mut [RoomIndex; STREAMED_ROOM_SLOT_COUNT],
    requested_count: usize,
    room: RoomIndex,
) -> usize {
    if requested_count >= STREAMED_ROOM_SLOT_COUNT
        || room == INVALID_ROOM_INDEX
        || room_requested(room, requested_rooms, requested_count)
        || !streamed_room_chunk_loadable(room)
    {
        return requested_count.min(STREAMED_ROOM_SLOT_COUNT);
    }
    requested_rooms[requested_count] = room;
    requested_count + 1
}

#[cfg(feature = "cd-stream-bench")]
fn streamed_request_sector_span(
    requested_rooms: &[RoomIndex; STREAMED_ROOM_SLOT_COUNT],
    requested_count: usize,
) -> Option<(u32, u32)> {
    let mut span_start = u32::MAX;
    let mut span_end = 0u32;
    let mut found = false;
    let mut i = 0usize;
    while i < requested_count.min(STREAMED_ROOM_SLOT_COUNT) {
        let room = requested_rooms[i];
        if room == INVALID_ROOM_INDEX {
            i += 1;
            continue;
        }
        let info = cd_stream::world_room_chunk_info(room.raw(), WORLD_PACK_TOC)?;
        if info.sector_count == 0
            || info.byte_size == 0
            || info.byte_size > STREAMED_ROOM_SLOT_BYTES
        {
            return None;
        }
        span_start = span_start.min(info.sector_offset);
        span_end = span_end.max(info.sector_offset.saturating_add(info.sector_count));
        found = true;
        i += 1;
    }
    if found {
        Some((span_start, span_end))
    } else {
        None
    }
}

const fn room_material_fallback() -> WorldRenderMaterial {
    WorldRenderMaterial::both(TextureMaterial::opaque(0, TPAGE_WORD, (0x80, 0x80, 0x80)))
}

#[cfg(feature = "world-grid-visible")]
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
    ) -> Option<(&[GridVisibleCell], u16)> {
        let sector_size = room_sector_size.max(1);
        let anchor_x = grid_cell_for_room(anchor.x, sector_size).clamp(0, room_width as i32 - 1);
        let anchor_z = grid_cell_for_room(anchor.z, sector_size).clamp(0, room_depth as i32 - 1);
        let (view_sin_key, view_cos_key) = visible_cell_view_keys(camera);
        let cache = *self.visible_cell_caches.get(active_slot)?;
        if cache.ready
            && cache.room == room_index
            && cache.anchor_x == anchor_x
            && cache.anchor_z == anchor_z
            && cache.view_sin_key == view_sin_key
            && cache.view_cos_key == view_cos_key
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

#[cfg(feature = "world-grid-visible")]
fn fill_precomputed_visible_cells(
    room_index: RoomIndex,
    anchor_x: i32,
    anchor_z: i32,
    room_offset_x: i32,
    room_offset_z: i32,
    sector_size: i32,
    global_anchor: RoomPoint,
    camera: WorldCamera,
    out: &mut [GridVisibleCell],
    depths: &mut [i32],
) -> Option<(usize, u16)> {
    let room_visibility = ROOM_VISIBILITY
        .iter()
        .find(|visibility| visibility.room == room_index)?;
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

#[cfg(feature = "world-grid-visible")]
fn visible_cell_view_keys(camera: WorldCamera) -> (i16, i16) {
    (
        (camera.sin_yaw.raw() / 256) as i16,
        (camera.cos_yaw.raw() / 256) as i16,
    )
}

#[cfg(feature = "world-grid-visible")]
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

#[cfg(feature = "world-grid-visible")]
fn visible_cell_camera_depth_if_sphere_visible(
    cell: psx_level::LevelVisibilityCellRecord,
    camera: WorldCamera,
    sector_size: i32,
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
    let far = FAR_Z.max(near);
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

#[cfg(feature = "world-grid-visible")]
#[derive(Copy, Clone)]
struct VisibleCellFilter {
    anchor_x: i32,
    anchor_z: i32,
    sector_size: i32,
    room_offset_x: i32,
    room_offset_z: i32,
    global_anchor: RoomPoint,
    camera: WorldCamera,
}

#[cfg(feature = "world-grid-visible")]
#[derive(Copy, Clone, PartialEq, Eq)]
enum VisibleCellReject {
    GlobalRange,
    Camera,
}

#[cfg(feature = "world-grid-visible")]
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
    let Some(depth) =
        visible_cell_camera_depth_if_sphere_visible(cell, filter.camera, filter.sector_size)
    else {
        return;
    };
    out[*written] = GridVisibleCell::with_cache_cell_index(
        cell.x,
        cell.z,
        cell.min_y,
        cell.max_y,
        cell.cache_cell_index,
    )
    .with_camera_depth(GridVisibleCell::CAMERA_DEPTH_PRECULLED);
    depths[*written] = depth;
    *written += 1;
}

#[cfg(feature = "world-grid-visible")]
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
    ) {
        return Some(VisibleCellReject::GlobalRange);
    }
    if !visibility_cell_aabb_intersects_camera(cell, filter.sector_size, filter.camera) {
        return Some(VisibleCellReject::Camera);
    }
    if !visibility_cell_in_view_wedge(cell, filter) {
        return Some(VisibleCellReject::Camera);
    }
    None
}

#[cfg(feature = "world-grid-visible")]
fn visibility_cell_safety_ring(
    cell: psx_level::LevelVisibilityCellRecord,
    anchor_x: i32,
    anchor_z: i32,
) -> bool {
    visibility_cell_anchor_distance(cell, anchor_x, anchor_z) <= ROOM_VISIBLE_CELL_SAFETY_RING
}

#[cfg(feature = "world-grid-visible")]
fn visibility_cell_anchor_distance(
    cell: psx_level::LevelVisibilityCellRecord,
    anchor_x: i32,
    anchor_z: i32,
) -> i32 {
    let dx = (cell.x as i32).saturating_sub(anchor_x).abs();
    let dz = (cell.z as i32).saturating_sub(anchor_z).abs();
    dx.max(dz)
}

#[cfg(feature = "world-grid-visible")]
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
    let dx = center_x.saturating_sub(anchor_x) as i64;
    let dz = center_z.saturating_sub(anchor_z) as i64;
    let sin_yaw = filter.camera.sin_yaw.raw() as i64;
    let cos_yaw = filter.camera.cos_yaw.raw() as i64;
    let forward_x = -sin_yaw;
    let forward_z = -cos_yaw;
    let depth = dx
        .saturating_mul(forward_x)
        .saturating_add(dz.saturating_mul(forward_z))
        >> 12;
    if depth < 0 {
        return anchor_distance <= ROOM_VISIBLE_CELL_REAR_RING;
    }
    let lateral = (dx
        .saturating_mul(cos_yaw)
        .saturating_sub(dz.saturating_mul(sin_yaw))
        >> 12)
        .unsigned_abs();
    let lateral_limit = (depth as u64)
        .saturating_mul(ROOM_VISIBLE_CELL_WEDGE_NUM)
        .checked_div(ROOM_VISIBLE_CELL_WEDGE_DEN.max(1))
        .unwrap_or(u64::MAX)
        .saturating_add(
            (sector_size as u64).saturating_mul(ROOM_VISIBLE_CELL_WEDGE_MARGIN_SECTORS as u64),
        );
    lateral <= lateral_limit
}

#[cfg(feature = "world-grid-visible")]
fn visibility_cell_aabb_intersects_camera(
    cell: psx_level::LevelVisibilityCellRecord,
    sector_size: i32,
    camera: WorldCamera,
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
    aabb_intersects_camera_frustum(x0, x1, y0, y1, z0, z1, camera)
}

#[cfg(feature = "world-grid-visible")]
fn aabb_intersects_camera_frustum(
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
    z0: i32,
    z1: i32,
    camera: WorldCamera,
) -> bool {
    let near = camera.projection.near_z.max(1);
    let far = FAR_Z.max(near);
    let focal = camera.projection.focal_length.max(1) as i64;
    let half_w = (camera.projection.screen_x as i32)
        .saturating_add(ROOM_VISIBLE_CELL_CAMERA_MARGIN)
        .max(1) as i64;
    let half_h = (camera.projection.screen_y as i32)
        .saturating_add(ROOM_VISIBLE_CELL_CAMERA_MARGIN)
        .max(1) as i64;
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
                let depth_limit_x = half_w.saturating_mul(view.z as i64);
                let depth_limit_y = half_h.saturating_mul(view.z as i64);
                let projected_x = (view.x as i64).saturating_mul(focal);
                let projected_y = (view.y as i64).saturating_mul(focal);
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

#[cfg(feature = "world-grid-visible")]
fn visibility_cell_in_global_range(
    x: u16,
    z: u16,
    sector_size: i32,
    room_offset_x: i32,
    room_offset_z: i32,
    global_anchor: RoomPoint,
) -> bool {
    let radius = ROOM_GLOBAL_VISIBILITY_RADIUS_SECTORS.saturating_mul(sector_size);
    let x0 = room_offset_x.saturating_add((x as i32).saturating_mul(sector_size));
    let z0 = room_offset_z.saturating_add((z as i32).saturating_mul(sector_size));
    let x1 = x0.saturating_add(sector_size);
    let z1 = z0.saturating_add(sector_size);
    rect_distance_sq(global_anchor.x, global_anchor.z, x0, x1, z0, z1)
        <= (radius as u64).saturating_mul(radius as u64)
}

#[cfg(feature = "world-grid-visible")]
fn visibility_pvs_bit(bits: &[u8], index: usize) -> bool {
    let byte = index / 8;
    let bit = index % 8;
    bits.get(byte)
        .map(|value| value & (1 << bit) != 0)
        .unwrap_or(false)
}

#[cfg(feature = "world-grid-visible")]
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

#[cfg(feature = "world-grid-visible")]
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

#[cfg(feature = "world-grid-visible")]
const fn visibility_cell_key(x: u16, z: u16) -> u32 {
    ((x as u32) << 16) | z as u32
}

const INVALID_ACTIVE_ROOM_SLOT: u8 = u8::MAX;

fn active_room_draw_order(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut depths = [i32::MIN; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
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

#[cfg(feature = "world-grid-visible")]
fn nearest_runtime_visibility_cell(
    cells: &[psx_level::LevelVisibilityCellRecord],
    x: i32,
    z: i32,
) -> Option<usize> {
    let mut best_index = None;
    let mut best_score = u64::MAX;
    for (index, cell) in cells.iter().enumerate() {
        let dx = (cell.x as i64).saturating_sub(x as i64).unsigned_abs();
        let dz = (cell.z as i64).saturating_sub(z as i64).unsigned_abs();
        let score = dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz));
        if best_index.is_none() || score < best_score {
            best_index = Some(index);
            best_score = score;
        }
    }
    best_index
}

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
    sin_key: i16,
    cos_key: i16,
}

impl ActiveRoomView {
    fn new(position: RoomPoint, yaw: Angle) -> Self {
        let sin_yaw = yaw.sin().raw();
        let cos_yaw = yaw.cos().raw();
        Self {
            position,
            sin_yaw,
            cos_yaw,
            sin_key: (sin_yaw / 512) as i16,
            cos_key: (cos_yaw / 512) as i16,
        }
    }
}

fn count_spatial_chunk_candidates(
    current_index: RoomIndex,
    current_record: &LevelRoomRecord,
    player: RoomPoint,
    view: ActiveRoomView,
) -> u16 {
    let mut count = 0u16;
    for chunk in ROOM_CHUNKS {
        if chunk.room == current_index
            || chunk_activation_score(*chunk, current_index, current_record, player, view).is_none()
        {
            continue;
        }
        count = count.saturating_add(1);
    }
    count
}

fn best_spatial_chunk_candidate(
    current_index: RoomIndex,
    current_record: &LevelRoomRecord,
    player: RoomPoint,
    view: ActiveRoomView,
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    skipped_rooms: &[RoomIndex],
) -> Option<RoomIndex> {
    let mut best = None;
    let mut best_score = ChunkActivationScore::WORST;
    for chunk in ROOM_CHUNKS {
        if chunk.room == current_index
            || active_room_window_contains(active_rooms, chunk.room)
            || skipped_rooms.contains(&chunk.room)
        {
            continue;
        }
        let Some(score) =
            chunk_activation_score(*chunk, current_index, current_record, player, view)
        else {
            continue;
        };
        if best.is_none() || score.better_than(best_score) {
            best_score = score;
            best = Some(chunk.room);
        }
    }
    best
}

fn best_spatial_chunk_candidate_from_indices(
    current_index: RoomIndex,
    current_record: &LevelRoomRecord,
    player: RoomPoint,
    view: ActiveRoomView,
    selected_rooms: &[RoomIndex],
) -> Option<RoomIndex> {
    let mut best = None;
    let mut best_score = ChunkActivationScore::WORST;
    for chunk in ROOM_CHUNKS {
        if chunk.room == current_index || selected_rooms.contains(&chunk.room) {
            continue;
        }
        let Some(score) =
            chunk_activation_score(*chunk, current_index, current_record, player, view)
        else {
            continue;
        };
        if best.is_none() || score.better_than(best_score) {
            best_score = score;
            best = Some(chunk.room);
        }
    }
    best
}

#[derive(Copy, Clone)]
struct ChunkActivationScore {
    tier: u8,
    view_tier: u8,
    ray_distance: u64,
    distance: u64,
}

impl ChunkActivationScore {
    const WORST: Self = Self {
        tier: u8::MAX,
        view_tier: u8::MAX,
        ray_distance: u64::MAX,
        distance: u64::MAX,
    };

    fn better_than(self, other: Self) -> bool {
        self.tier < other.tier
            || (self.tier == other.tier
                && (self.view_tier < other.view_tier
                    || (self.view_tier == other.view_tier
                        && (self.ray_distance < other.ray_distance
                            || (self.ray_distance == other.ray_distance
                                && self.distance < other.distance)))))
    }
}

fn chunk_activation_score(
    chunk: LevelChunkRecord,
    current_index: RoomIndex,
    current_record: &LevelRoomRecord,
    player: RoomPoint,
    view: ActiveRoomView,
) -> Option<ChunkActivationScore> {
    if !chunk_within_activation_range(chunk, current_record, player) {
        return None;
    }
    let current_authored_room = authored_room_for_chunk(current_index)?;
    let same_authored = chunk.authored_room == current_authored_room;
    if !same_authored
        && authored_room_for_chunk(current_index)
            .and_then(|authored| authored_bounds_current_space(authored, current_record))
            .is_some_and(|bounds| {
                rects_overlap(chunk_bounds_current_space(chunk, current_record), bounds)
            })
    {
        return None;
    }
    let distance = chunk_distance_sq_current_space(chunk, current_record, player);
    let (view_tier, ray_distance) = chunk_view_score_current_space(chunk, current_record, view);
    Some(ChunkActivationScore {
        tier: if same_authored { 0 } else { 1 },
        view_tier,
        ray_distance,
        distance,
    })
}

fn chunk_view_score_current_space(
    chunk: LevelChunkRecord,
    current_record: &LevelRoomRecord,
    view: ActiveRoomView,
) -> (u8, u64) {
    let (x0, x1, z0, z1) = chunk_bounds_current_space(chunk, current_record);
    let center_x = x0.saturating_add((x1.saturating_sub(x0)) / 2);
    let center_z = z0.saturating_add((z1.saturating_sub(z0)) / 2);
    let radius = (x1.saturating_sub(x0).abs()).max(z1.saturating_sub(z0).abs()) / 2;
    let dx = center_x.saturating_sub(view.position.x) as i64;
    let dz = center_z.saturating_sub(view.position.z) as i64;
    let forward_x = -(view.sin_yaw as i64);
    let forward_z = -(view.cos_yaw as i64);
    let depth = (dx
        .saturating_mul(forward_x)
        .saturating_add(dz.saturating_mul(forward_z)))
        >> 12;
    let lateral = (dx
        .saturating_mul(view.cos_yaw as i64)
        .saturating_sub(dz.saturating_mul(view.sin_yaw as i64))
        >> 12)
        .unsigned_abs();
    let radius = radius.max(0) as u64;
    let lateral_gap = lateral.saturating_sub(radius);
    let behind_gap = if depth < 0 { (-depth) as u64 } else { 0 };
    let ray_distance = lateral_gap
        .saturating_mul(lateral_gap)
        .saturating_add(behind_gap.saturating_mul(behind_gap));
    let cone_limit = (depth.max(0) as u64)
        .saturating_add(radius)
        .saturating_add((current_record.sector_size.max(1) as u64).saturating_mul(2));
    let view_tier = if depth.saturating_add(radius as i64) < 0 {
        3
    } else if depth < (current_record.sector_size.max(1) as i64).saturating_mul(4)
        && lateral > radius / 2
    {
        1
    } else if lateral <= cone_limit {
        0
    } else if ray_distance
        <= (current_record.sector_size.max(1) as u64)
            .saturating_mul(current_record.sector_size.max(1) as u64)
            .saturating_mul(16)
    {
        1
    } else {
        2
    };
    (view_tier, ray_distance)
}

fn authored_room_for_chunk(index: RoomIndex) -> Option<u32> {
    chunk_record_for_room(index).map(|chunk| chunk.authored_room)
}

fn chunk_record_for_room(index: RoomIndex) -> Option<&'static LevelChunkRecord> {
    ROOM_CHUNKS.iter().find(|chunk| chunk.room == index)
}

fn authored_bounds_current_space(
    authored_room: u32,
    current_record: &LevelRoomRecord,
) -> Option<(i32, i32, i32, i32)> {
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    for chunk in ROOM_CHUNKS {
        if chunk.authored_room != authored_room {
            continue;
        }
        bounds = Some(match bounds {
            Some((ax0, ax1, az0, az1)) => {
                let (bx0, bx1, bz0, bz1) = chunk_bounds_current_space(*chunk, current_record);
                (ax0.min(bx0), ax1.max(bx1), az0.min(bz0), az1.max(bz1))
            }
            None => chunk_bounds_current_space(*chunk, current_record),
        });
    }
    bounds
}

fn rects_overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    let (ax0, ax1, az0, az1) = a;
    let (bx0, bx1, bz0, bz1) = b;
    ax0 < bx1 && ax1 > bx0 && az0 < bz1 && az1 > bz0
}

fn chunk_within_activation_range(
    chunk: LevelChunkRecord,
    current_record: &LevelRoomRecord,
    player: RoomPoint,
) -> bool {
    let sector_size = current_record.sector_size.max(1);
    let radius = ROOM_GLOBAL_VISIBILITY_RADIUS_SECTORS.saturating_mul(sector_size);
    chunk_distance_sq_current_space(chunk, current_record, player)
        <= (radius as u64).saturating_mul(radius as u64)
}

fn chunk_distance_sq_current_space(
    chunk: LevelChunkRecord,
    current_record: &LevelRoomRecord,
    player: RoomPoint,
) -> u64 {
    let (x0, x1, z0, z1) = chunk_bounds_current_space(chunk, current_record);
    rect_distance_sq(player.x, player.z, x0, x1, z0, z1)
}

fn chunk_bounds_current_space(
    chunk: LevelChunkRecord,
    current_record: &LevelRoomRecord,
) -> (i32, i32, i32, i32) {
    let sector_size = current_record.sector_size.max(1);
    let x0 = chunk
        .origin_x
        .saturating_sub(current_record.origin_x)
        .saturating_mul(sector_size);
    let z0 = chunk
        .origin_z
        .saturating_sub(current_record.origin_z)
        .saturating_mul(sector_size);
    let x1 = x0.saturating_add((chunk.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((chunk.depth as i32).saturating_mul(sector_size));
    (x0, x1, z0, z1)
}

#[cfg(feature = "cd-stream-bench")]
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

fn rect_distance_sq(x: i32, z: i32, x0: i32, x1: i32, z0: i32, z1: i32) -> u64 {
    let dx = if x < x0 {
        x0 - x
    } else if x > x1 {
        x - x1
    } else {
        0
    };
    let dz = if z < z0 {
        z0 - z
    } else if z > z1 {
        z - z1
    } else {
        0
    };
    (dx as u64)
        .saturating_mul(dx as u64)
        .saturating_add((dz as u64).saturating_mul(dz as u64))
}

fn point_xz_distance_sq(a: RoomPoint, b: RoomPoint) -> u64 {
    let dx = (a.x as i64).saturating_sub(b.x as i64).unsigned_abs();
    let dz = (a.z as i64).saturating_sub(b.z as i64).unsigned_abs();
    dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz))
}

fn room_bounds(record: &LevelRoomRecord, room: RuntimeRoom<'_>) -> (i32, i32, i32, i32) {
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let x1 = x0.saturating_add((room.width() as i32).saturating_mul(record.sector_size));
    let z1 = z0.saturating_add((room.depth() as i32).saturating_mul(record.sector_size));
    (x0, x1, z0, z1)
}

fn rooms_touch(
    a_record: &LevelRoomRecord,
    a_room: RuntimeRoom<'_>,
    b_record: &LevelRoomRecord,
    b_room: RuntimeRoom<'_>,
) -> bool {
    let (ax0, ax1, az0, az1) = room_bounds(a_record, a_room);
    let (bx0, bx1, bz0, bz1) = room_bounds(b_record, b_room);
    bx0 <= ax1 && bx1 >= ax0 && bz0 <= az1 && bz1 >= az0
}

fn nearest_touching_room_index(
    current_index: RoomIndex,
    current_record: &LevelRoomRecord,
    current_room: RuntimeRoom<'_>,
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
) -> Option<usize> {
    let mut best_index = None;
    let mut best_score = u64::MAX;
    for (raw_index, record) in ROOMS.iter().enumerate() {
        let index = RoomIndex::new(raw_index as u16);
        if index == current_index || active_room_window_contains(active_rooms, index) {
            continue;
        }
        let Some(room) = parse_runtime_room(record) else {
            continue;
        };
        if !rooms_touch(current_record, current_room, record, room) {
            continue;
        }
        let score = room_center_distance_sq(current_record, current_room, record, room);
        if score < best_score {
            best_score = score;
            best_index = Some(raw_index);
        }
    }
    best_index
}

fn active_room_window_contains(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    index: RoomIndex,
) -> bool {
    active_rooms
        .iter()
        .flatten()
        .any(|active| active.index == index)
}

fn room_center_distance_sq(
    a_record: &LevelRoomRecord,
    a_room: RuntimeRoom<'_>,
    b_record: &LevelRoomRecord,
    b_room: RuntimeRoom<'_>,
) -> u64 {
    let (ax0, ax1, az0, az1) = room_bounds(a_record, a_room);
    let (bx0, bx1, bz0, bz1) = room_bounds(b_record, b_room);
    let acx = (ax0 as i64 + ax1 as i64) / 2;
    let acz = (az0 as i64 + az1 as i64) / 2;
    let bcx = (bx0 as i64 + bx1 as i64) / 2;
    let bcz = (bz0 as i64 + bz1 as i64) / 2;
    let dx = acx - bcx;
    let dz = acz - bcz;
    dx.unsigned_abs()
        .saturating_mul(dx.unsigned_abs())
        .saturating_add(dz.unsigned_abs().saturating_mul(dz.unsigned_abs()))
}

fn room_index_containing_global(point: RoomPoint) -> Option<RoomIndex> {
    if !ROOM_CHUNKS.is_empty() {
        for chunk in ROOM_CHUNKS {
            let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
                continue;
            };
            let sector_size = record.sector_size.max(1);
            let x0 = room_origin_x(record);
            let z0 = room_origin_z(record);
            let x1 = x0.saturating_add((chunk.width as i32).saturating_mul(sector_size));
            let z1 = z0.saturating_add((chunk.depth as i32).saturating_mul(sector_size));
            if point.x >= x0 && point.x < x1 && point.z >= z0 && point.z < z1 {
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
        material.with_tint(self.shade_tint_at(RoomPoint::from_world_vertex(point), material.tint()))
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
        let depth = self.camera.view_vertex(point.to_world_vertex()).z;
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
            self.shade_vertex(sample, RoomPoint::from_world_vertex(vertices[0]), material),
            self.shade_vertex(sample, RoomPoint::from_world_vertex(vertices[1]), material),
            self.shade_vertex(sample, RoomPoint::from_world_vertex(vertices[2]), material),
            self.shade_vertex(sample, RoomPoint::from_world_vertex(vertices[3]), material),
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
                RoomPoint::from_world_vertex(vertices[0]),
                material.texture.tint(),
                depths[0],
            ),
            self.shade_tint_at_depth(
                RoomPoint::from_world_vertex(vertices[1]),
                material.texture.tint(),
                depths[1],
            ),
            self.shade_tint_at_depth(
                RoomPoint::from_world_vertex(vertices[2]),
                material.texture.tint(),
                depths[2],
            ),
            self.shade_tint_at_depth(
                RoomPoint::from_world_vertex(vertices[3]),
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

fn room_fog_weight(depth: i32, enabled: bool, fog_near: i32, fog_far: i32) -> i32 {
    if !enabled || fog_far <= fog_near || depth <= fog_near {
        return 0;
    }
    (((depth - fog_near).saturating_mul(256)) / (fog_far - fog_near)).clamp(0, 256)
}

fn apply_room_fog_weight(tint: (u8, u8, u8), fog_rgb: Rgb8, weight: i32) -> (u8, u8, u8) {
    if weight <= 0 {
        return tint;
    }
    let keep = 256 - weight;
    (
        blend_channel(tint.0, fog_rgb.r, keep, weight),
        blend_channel(tint.1, fog_rgb.g, keep, weight),
        blend_channel(tint.2, fog_rgb.b, keep, weight),
    )
}

fn blend_channel(src: u8, fog: u8, keep: i32, weight: i32) -> u8 {
    (((src as i32) * keep + (fog as i32) * weight) >> 8).clamp(0, 255) as u8
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
            .find(|s| s.asset == asset_id && s.clut_mode == clut_mode)
    }
}

fn find_room_texture_vram_slot(asset_id: AssetId) -> Option<VramSlot> {
    unsafe {
        VRAM_SLOTS.iter().filter_map(|s| *s).find(|s| {
            s.asset == asset_id
                && matches!(
                    s.clut_mode,
                    VramSlotClutMode::OpaqueZero | VramSlotClutMode::TransparentZero
                )
        })
    }
}

fn ensure_texture_uploaded(asset_id: AssetId, asset_bytes: &[u8]) -> Option<VramSlot> {
    ensure_texture_uploaded_with_clut_mode(asset_id, asset_bytes, VramSlotClutMode::OpaqueZero)
}

fn ensure_texture_uploaded_with_clut_mode(
    asset_id: AssetId,
    asset_bytes: &[u8],
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
        telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
        telemetry::counter(telemetry::counter::ROOM_TEXTURE_UPLOADS, 1);
        let clut_rect = VramRect::new(clut_x, ROOM_CLUT_Y, texture.clut_entries(), 1);
        if clut_mode == VramSlotClutMode::OpaqueZero {
            upload_opaque_clut(clut_rect, texture.clut_bytes());
        } else {
            upload_clut(clut_rect, texture.clut_bytes());
        }
        telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);

        let slot = VramSlot {
            asset: asset_id,
            clut_mode,
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
            let _ = RESIDENCY.mark_vram_resident(asset_id);
        }
        return Some(slot);
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
    if end_x > ROOM_TPAGE_LIMIT_X {
        return None;
    }
    let tpage = Tpage::new(tpage_x, SHARED_TPAGE.y(), TexDepth::Bit4);
    let texture_x = tpage_x.checked_add(u16::from(placement.origin_u()) / 4)?;
    let texture_y = SHARED_TPAGE
        .y()
        .checked_add(u16::from(placement.origin_v()))?;

    telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
    telemetry::counter(telemetry::counter::ROOM_TEXTURE_UPLOADS, 1);
    if !upload_4bpp_tile(
        texture_x,
        texture_y,
        texture_width_halfwords,
        texture_height_rows,
        &texture,
    ) {
        telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);
        return None;
    }

    let clut_rect = VramRect::new(clut_x, ROOM_CLUT_Y, texture.clut_entries(), 1);
    if clut_mode == VramSlotClutMode::OpaqueZero {
        upload_opaque_clut(clut_rect, texture.clut_bytes());
    } else {
        upload_clut(clut_rect, texture.clut_bytes());
    }
    telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);

    let clut = Clut::new(clut_x, ROOM_CLUT_Y);
    let slot = VramSlot {
        asset: asset_id,
        clut_mode,
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
        // Mirror VRAM into the residency tracker. mark_vram_resident
        // returns false if it overflows; we already reserved a
        // slot so this should always succeed.
        let _ = RESIDENCY.mark_vram_resident(asset_id);
    }

    Some(slot)
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
    if tpage_x.checked_add(slot_halfwords)? > MODEL_TPAGE_LIMIT_X {
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
    let shadow_options = WorldSurfaceOptions::new(WORLD_BAND, WORLD_DEPTH_RANGE)
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
    elapsed_vblanks: u32,
    video_hz: u16,
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
        let phase = anim.phase_at_tick_q12(elapsed_vblanks, video_hz);

        // Instance Y-axis rotation from authored yaw. PSX angle
        // units (4096 per turn) → Q12 sin/cos via the existing
        // GTE shim, then composed into a rotation matrix.
        let instance_rotation = yaw_rotation_matrix(Angle::from_q12(inst.yaw as u16));
        // Authored instance positions are floor anchors; cooked
        // model vertices are centred around their bounds.
        let origin = visual_model_origin(
            inst.x,
            inst.y,
            inst.z,
            runtime_model.world_height,
            inst.visual_offset,
            inst.visual_scale_q8,
            &instance_rotation,
        );
        let local_to_world = visual_model_local_to_world(runtime_model, inst.visual_scale_q8);
        if !depth_pass.includes(camera.view_vertex(origin).z) {
            continue;
        }
        telemetry::stage_begin(telemetry::stage::MODEL_BOUNDS);
        out.bounds_tests = out.bounds_tests.saturating_add(1);
        let visible = match model_frame_bounds(runtime_model, clip_local, phase) {
            Some(bounds) if MODEL_BOUNDS_CULLING_ENABLED => model_bounds_visible(
                camera,
                options,
                origin,
                instance_rotation,
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
            instance_rotation,
            local_to_world,
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

fn draw_image_props(
    props: &[LevelImagePropRecord],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    for prop in props {
        if prop.room != current_room {
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
        let origin = WorldVertex::new(prop.x, prop.y, prop.z);
        let center = WorldVertex::new(
            prop.x,
            prop.y.saturating_add((prop.height as i32) / 2),
            prop.z,
        );
        let radius = ((prop.width.max(prop.height) as i32) / 2).max(32);
        if !sphere_visible_to_camera(camera, options, center, radius, 96) {
            continue;
        }
        let sort_depth = camera.view_vertex(center).z;
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
        let tint = lighting.shade_tint_at(
            RoomPoint::new(prop.x, prop.y, prop.z),
            rgb_tuple(prop.tint_rgb),
        );
        let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, tint)
            .with_texture_window(slot.texture_window);
        let u_max = model_render_uv_max(slot.texture_width);
        let v_max = model_render_uv_max(slot.texture_height);
        let uvs = [(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)];
        let opts = options
            .with_depth_policy(DepthPolicy::Fixed(sort_depth))
            .with_depth_bias(options.depth_bias.saturating_sub(IMAGE_PROP_DEPTH_BIAS))
            .with_cull_mode(CullMode::None)
            .with_material_layer(material)
            .with_textured_triangle_splitting(true)
            .with_textured_triangle_max_edge(0);
        let _ = world.submit_textured_world_quad(triangles, *camera, verts, uvs, material, opts);
    }
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
    (((value as i64) * (q12 as i64)) >> 12).clamp(i32::MIN as i64, i32::MAX as i64) as i32
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

fn draw_lock_target_indicator(target: RoomPoint, camera: WorldCamera, elapsed_vblanks: u32) {
    let Some(center) = camera.project_world(target.to_world_vertex()) else {
        return;
    };

    let outer = TARGET_LOCK_OUTER;
    let inner = TARGET_LOCK_INNER;
    let half_width = TARGET_LOCK_TRI_HALF_WIDTH;
    let angle = Angle::per_frames(TARGET_LOCK_ROTATION_FRAMES).mul_frame(elapsed_vblanks);
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

fn playtest_visual_pacing(video_mode: VideoMode) -> VisualPacing {
    match video_mode {
        VideoMode::Ntsc => VisualPacing::EveryNVBlanks(3),
        // PAL is 50Hz, so exact 20Hz pacing does not divide cleanly.
        // Use a deterministic 25Hz fallback instead of a jittery 2/3
        // cadence; PAL-specific 20Hz interpolation can be added later.
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
        ..Config::default()
    };
    App::run(config, &mut scene);
}
