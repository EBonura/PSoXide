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

mod active_room_cache;
mod active_room_streaming;
mod active_rooms;
mod box_props;
#[cfg(feature = "cd-stream-bench")]
mod cd_stream;
mod character_runtime;
mod debug_runtime;
mod image_props_runtime;
mod input;
mod marker_runtime;
mod model_rendering;
mod overlay;
mod particle_runtime;
mod playtest_scene;
mod room_lighting_runtime;
mod runtime_config;
mod runtime_schedule;
mod sky_runtime;
mod visibility_runtime;
mod vram_runtime;
mod vram_upload;

use active_room_cache::*;
use active_room_streaming::*;
use box_props::*;
use character_runtime::*;
use debug_runtime::*;
use image_props_runtime::*;
use input::*;
use marker_runtime::*;
use model_rendering::*;
use overlay::*;
use particle_runtime::*;
use room_lighting_runtime::*;
use runtime_config::*;
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
