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
//! * CIRCLE tap        -- directional roll; lock-on remains active.
//! * CIRCLE hold       -- run while moving.
//! * R1                -- light attack.
//! * R2                -- heavy attack.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate alloc;
extern crate psx_rt;

#[cfg(all(target_arch = "mips", feature = "boot-trace"))]
fn game_trace(message: &str) {
    psx_rt::tty::println(message);
}

#[cfg(not(all(target_arch = "mips", feature = "boot-trace")))]
fn game_trace(_message: &str) {}

use psx_asset::{Animation, ModelPart, ModelVertex};
// Used by the vis-full-active-chunks default AND by the PVS path's
// no-anchor fallback (a far room with no usable portal anchor draws
// every cell through the cached path).
#[cfg(feature = "world-grid-visible")]
use psx_engine::draw_indexed_cached_room_vertex_lit_all_cells;
use psx_engine::draw_room_vertex_lit;
use psx_engine::ui::UiTextureSlot;
use psx_engine::world_render::PortalCellWindow;
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
    button, horizontal_view_coordinates, prewarm_indexed_cached_room_quads, telemetry,
    AdaptiveSubdivisionKindMask, Angle, App, CachedRoomCell, CachedRoomDepthMode,
    CachedRoomSubdivisionMode, CachedRoomSurface, CharacterCollision, CharacterCollisionAabb,
    CharacterCollisionCylinder, CharacterCollisionRoom, CharacterMotorAnim, CharacterMotorConfig,
    CharacterMotorInput, CharacterMotorState, Config, Ctx, DepthBand, DepthRange,
    LoadedWorldCameraGte, OtFrame, PrimitivePacketArena, PrimitivePacketScratch, PrimitiveSink,
    ProjectedVertex, RenderSubmission, Rgb8, RoomPoint, RuntimeCollisionRoom, RuntimeRoom, Scene,
    SceneStateRef, SchedulerConfig, SimTick, TexturedModelRenderFace, ThirdPersonCameraConfig,
    ThirdPersonCameraInput, ThirdPersonCameraState, ThirdPersonCameraTarget, VideoHz, VisualPacing,
    WorldCamera, WorldProjection, WorldRenderMaterial, WorldRenderPass, WorldSurfaceOptions,
    WorldTriCommand, WorldVertex, Q12,
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
    draw_tri_flat_blended,
    material::{BlendMode, TextureMaterial},
    ot::OrderingTable,
    prim::{QuadTexturedGouraud, TriTextured, TriTexturedGouraud},
    VideoMode,
};
use psx_level::portal_visibility::{
    debug_portal_clip, PortalClipDebug, PortalClipDebugDecision, PortalClipDebugPlane,
    PortalClipDebugRect, PortalFrustum, PortalRoomBounds, PortalVisibilityCamera,
    PortalVisibilityResult,
};
use psx_level::{
    find_asset_of_kind, room_flags, AssetId, AssetKind, CharacterAnimationAction, EntityRecord,
    InteractableKind, InteractableRecord, LevelBoxPropRecord, LevelCameraRecord,
    LevelCharacterRecord, LevelChunkRecord, LevelFarVistaRecord, LevelGameEntityRecord,
    LevelImagePropRecord, LevelRoomRecord, LevelSkyRecord, LevelWaterCellRecord, ModelClipIndex,
    ParticleEmitterRecord, RoomIndex, RuntimeDebugMask,
};
use psx_vram::{TexDepth, Tpage};

mod active_room_cache;
mod active_room_streaming;
mod active_room_visibility;
mod active_rooms;
mod box_props;
mod bsp_runtime;
#[cfg(feature = "cd-stream-benchmark")]
use psx_game_runtime::cd_stream;
mod character_runtime;
mod debug_runtime;
mod game_logic_runtime;
mod image_props_runtime;
mod input;
mod marker_runtime;
mod model_rendering;
mod overlay;
mod particle_runtime;
mod playtest_runtime;
mod playtest_scene;
mod playtest_update;
mod room_lighting_runtime;
mod runtime_arenas;
mod runtime_config;
mod runtime_schedule;
mod sky_runtime;
mod visibility_runtime;
mod visible_cell_runtime;
mod vram_runtime;
mod water_runtime;

use active_room_cache::*;
use active_room_streaming::*;
use box_props::*;
use bsp_runtime::*;
use character_runtime::*;
use debug_runtime::*;
use game_logic_runtime::*;
use image_props_runtime::*;
use input::*;
use marker_runtime::*;
use model_rendering::*;
use overlay::*;
use particle_runtime::*;
use room_lighting_runtime::*;
use runtime_arenas::*;
use runtime_config::*;
use runtime_schedule::RUNTIME_SCHEDULE;
use sky_runtime::*;
use visibility_runtime::*;
use visible_cell_runtime::*;
use vram_runtime::*;
use water_runtime::*;

// Placeholder manifests reference unused statics; populated
// manifests reference all of them. Quiet either side here.
#[allow(dead_code, unused_imports)]
mod generated {
    include!(env!("PSXED_PLAYTEST_MANIFEST"));
}

use generated::{
    ARCH_PROPS, ARCH_PROP_COLLISIONS, ARCH_PROP_SURFACES, ASSETS, BOX_PROPS, BOX_PROP_STATE_COUNT,
    BOX_PROP_SURFACES, CACHED_ROOM_DEPTH_MODE, CACHED_ROOM_DRAW_ORDER_MODE,
    CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE, CACHED_ROOM_TEXTURE_SPLIT_MODE, CHARACTERS,
    COMBAT_CAPSULES, CYLINDER_PROPS, CYLINDER_PROP_SURFACES, ENTITIES, EQUIPMENT, GAME_ENTITIES,
    IMAGE_PROPS, INTERACTABLES, INTERACTABLE_MESSAGES, LIGHTS, LOGIC, MATERIALS, MODELS,
    MODEL_CLIPS, MODEL_CLIP_BOUNDS, MODEL_FRAME_BOUNDS, MODEL_INSTANCES, MODEL_SOCKETS,
    PARTICLE_EMITTERS, PLAYER_CONTROLLER, PLAYER_SPAWN, PLAYTEST_PACKET_CAPACITY,
    PXBSP_AMBIENT_RGB, ROOMS, ROOM_CACHE_CELLS, ROOM_CACHE_CELL_VERTICES, ROOM_CACHE_SURFACES,
    ROOM_CACHE_VERTICES, ROOM_CHUNKS, ROOM_OVERLAPPED_ROOMS, ROOM_PORTALS, ROOM_REFLECTION_PROBES,
    ROOM_RESIDENCY, ROOM_SURFACE_CACHES, ROOM_VISIBILITY, UI_FONTS, UI_NODES, UI_PAINTS,
    UI_SFX_CUES, UI_SFX_SAMPLES, VISIBILITY_CELLS, WATER_CELLS, WEAPONS, WEAPON_HITBOXES,
};
#[cfg(feature = "cd-stream-bench")]
use generated::{
    GAMEPLAY_PACK_MAX_CHUNK_BYTES, PERSISTENT_ASSET_PAGE_COUNT, PERSISTENT_ASSET_SLOT_COUNT,
    UI_PACK_IMAGE_CACHE_SLOTS, UI_PACK_MAX_CHUNK_BYTES, UI_PACK_START_LBA, UI_PACK_TOC,
    WORLD_PACK_MAX_CHUNK_BYTES, WORLD_PACK_START_LBA, WORLD_PACK_TOC, WORLD_RESIDENT_CHUNK_LIMIT,
    WORLD_RESIDENT_PAGE_COUNT, WORLD_STREAM_SLOT_COUNT,
};
use generated::{GAME_FLOW, OPTIONS, UI_SCENES};
#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
use generated::{VISIBILITY_PVS, VISIBILITY_PVS_BITS};

static mut OT: OrderingTable<OT_DEPTH> = OrderingTable::new();
static mut PRIMITIVE_PACKETS: PrimitivePacketScratch<MAX_TEXTURED_TRIS> =
    PrimitivePacketScratch::ZERO;
static mut WORLD_COMMANDS: [WorldTriCommand; MAX_TEXTURED_TRIS] =
    [WorldTriCommand::EMPTY; MAX_TEXTURED_TRIS];
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
    let ax = abs_i32(sin_yaw);
    let az = abs_i32(cos_yaw);
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

/// Locomotion crossfade window in sim ticks (60 Hz): long enough to
/// soften idle/walk/run cuts, short enough that matrix-lerp shrink
/// stays invisible.
const PLAYER_ANIM_BLEND_LOCOMOTION_TICKS: u32 = 8;
/// Attack/action crossfade window in sim ticks: snappier so combat
/// startup frames are not softened away.
const PLAYER_ANIM_BLEND_ACTION_TICKS: u32 = 4;
/// Crossfade window when LEAVING a committed one-shot (attack, intro).
/// Entering one stays snappy so its startup frames read, but a 2.5-4.5s
/// swing cut to idle in four ticks is the harshest transition in the game;
/// it wants a settle roughly as long as a footfall.
const PLAYER_ANIM_BLEND_ACTION_OUT_TICKS: u32 = 14;
/// Crossfade window between two stepping gaits (walk <-> run <-> strafe).
/// Longer than the generic locomotion window because gait clips are
/// cook-time aligned to foot-down and the incoming clip is entered at the
/// outgoing clip's phase, so a long fade blends like poses instead of
/// averaging opposite halves of a stride.
const PLAYER_ANIM_BLEND_GAIT_TICKS: u32 = 10;

struct Playtest {
    /// Active room. `None` until `init` runs and only `Some`
    /// when the manifest had at least one room and its bytes
    /// parsed.
    room: Option<RuntimeRoom<'static>>,
    /// Explicit resident BSP backend selected by the cooked manifest. `None`
    /// means the project selected the legacy grid world; invalid BSP data
    /// fails initialization and never reaches this fallback state.
    bsp: Option<BspRuntime>,
    /// Active collision room. Streamed builds use a compact
    /// collision-only payload here instead of a full `.psxw`.
    current_collision_room: Option<RuntimeCollisionRoom<'static>>,
    /// Ambient RGB for the room containing the player.
    current_ambient_rgb: [u8; 3],
    /// Active-room window runtime state (the cache-budgeted draw
    /// chunks, the incremental rebuild job staged against them, the
    /// request anchor, and skip diagnostics), owned by
    /// `psx_game_runtime::room_window` since the phase-1 carve.
    window: RuntimeRoomWindow,
    /// Portal-visibility runtime state (traversal result, root, bounds
    /// cache, view keys, per-refresh diagnostics), owned by
    /// `psx_game_runtime::room_visibility` since the phase-1 carve.
    visibility: RuntimeRoomVisibility,
    portal_stream_priority_current: u16,
    portal_stream_priority_visible: u16,
    portal_stream_priority_frontier: u16,
    /// Per-active-slot visible-cell caches over one shared pool, owned
    /// by `psx_game_runtime::world_cells` since the phase-2 carve.
    #[cfg(all(
        feature = "world-grid-visible",
        not(feature = "vis-full-active-chunks")
    ))]
    visible_cells: RuntimeVisibleCellSelector,
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
    /// Active clip-transition crossfade: outgoing state, its frozen
    /// clip-local tick, and the switch tick the blend ramps from.
    /// Cleared on init/respawn; expires by elapsed ticks at render.
    anim_blend_from: Option<(PlayerAnim, u32, SimTick)>,
    /// Active-window reconcile needed: set by visibility refreshes,
    /// crossings, and stream progress; cleared when a pass converges.
    /// Keeps the steady-state reconcile at a two-branch early-out.
    active_window_dirty: bool,
    /// Breakable box-prop state (broken bits, derived data, falls,
    /// break bursts), owned by `psx_game_runtime::box_props` since the
    /// phase-2 carve.
    box_props: RuntimeBoxProps,
    /// Souls-like game-entity SoA state over the cooked
    /// `GAME_ENTITIES` records (phase 3; empty for record-free
    /// projects and then inert).
    game_entities: RuntimeGameEntities,
    /// Exact attack clip/phase tokens emitted by the latest NPC tick. The
    /// post-update pose pass consumes these before player melee, keeping enemy
    /// body, equipment, and contact on one retained sample.
    deferred_enemy_attacks: RuntimeDeferredEnemyAttacks,
    /// Logic-entity runtime (delay queue, master gating, fan-out)
    /// over the cooked `LOGIC` records (phase 3).
    logic: RuntimeLogic,
    /// Rolling fired total already reported to telemetry, so the
    /// LOGIC_RECORDS_FIRED counter emits per-tick deltas.
    logic_fired_reported: u16,
    /// Player health (phase-3 combat slice). Stamped to
    /// `PLAYER_MAX_HEALTH` at gameplay init; entity attack
    /// connections subtract from it; floors at 0 (death/respawn is
    /// phase 4).
    player_health: u16,
    /// See [`Self::player_health`].
    player_health_max: u16,
    /// Remaining deep-water death sequence ticks. Non-zero locks player
    /// input while gravity carries the body below the authored surface.
    water_death_ticks_remaining: u8,
    /// Entities already hit by the CURRENT player swing (bit i =
    /// entity i): one swing connects at most once per enemy. Cleared
    /// when an attack action starts.
    // psx-numeric-allow-next-line: one-hit-per-swing bitmask over 64 entity records; bit ops only, two-word on R3000
    swing_hit_mask: u64,
    /// Player skeleton/presentation state frozen after the latest fixed update.
    /// Body rendering, equipment sockets, and authored combat capsules all
    /// consume this same snapshot until the next simulation tick.
    player_actor_pose: Option<PlayerActorPoseSnapshot>,
    /// Per-cooked-instance pose authority for the latest fixed update. The
    /// table index is the `MODEL_INSTANCES` index, covering both live game
    /// entities and static placed actors without render-time resampling.
    instance_actor_poses: [Option<InstanceActorPoseSnapshot>; MAX_MODEL_INSTANCES],
    /// PXBSP PVS result for cooked model instances. Grid worlds leave this at
    /// all-visible and continue using their room window.
    bsp_instance_visible_mask: u16,
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
    /// Camera snapshot taken by `render` for the deferred overlay pass.
    /// The flip (and so `render_overlay`) runs after the next fixed
    /// update has already moved `render_camera`; world-anchored overlay
    /// elements must use the camera the underlying frame was built with.
    overlay_camera: WorldCamera,
    /// Sim-tick snapshot taken by `render` alongside `overlay_camera`.
    /// Time-animated overlay elements (atmosphere particles, lock-on
    /// indicator) must use the tick of the frame they decorate: the
    /// tick current at flip time depends on deadline-miss cadence, so
    /// sampling it made the overlay's animation phase nondeterministic
    /// across builds (the 3-pixel corridor LSB instability).
    overlay_sim_tick: SimTick,
    /// Overlay state for the frame currently being prepared on the CPU. It is
    /// promoted to `overlay_*` only when that frame is submitted, after the
    /// previous queued frame has used its own snapshot.
    prepared_overlay_camera: WorldCamera,
    prepared_overlay_sim_tick: SimTick,
    prepared_overlay_analog: bool,
    overlay_analog: bool,
    /// Tick of the first gameplay update after loading completed. The
    /// engine clock origin is set at app init, BEFORE CD loading, so
    /// raw `ctx.sim_tick` VALUES carry the build- and disc-dependent
    /// loading duration. Gameplay logic is immune (it compares ticks
    /// from the same clock), but value-based animation phase (ambient
    /// model instances, particles, atmosphere) must subtract this
    /// epoch so visuals are a pure function of gameplay time.
    gameplay_epoch: SimTick,
    gameplay_epoch_set: bool,
    /// fps-overlay counter state (burn builds): presented frames and
    /// worst inter-frame gap over a rolling ~1s gameplay-tick window.
    fps_window_start: u32,
    fps_window_frames: u8,
    fps_last_tick: u32,
    fps_worst_gap: u8,
    fps_display: u8,
    fps_display_worst: u8,
    /// Cached camera collision-room set: the follow camera's per-tick
    /// room gather cost ~half of its 50k tick budget and the set only
    /// changes when the player crosses a coarse cell or the active
    /// window changes (see `camera_rooms_key`).
    camera_collision_rooms: [CharacterCollisionRoom<'static>; MAX_COLLISION_ROOMS],
    camera_collision_room_count: usize,
    /// (current room, player cell x/z at the cache quantum, active-room
    /// mask) the cached camera room set was gathered for.
    camera_rooms_key: (RoomIndex, i32, i32, u32, u32),
    /// Last movement result; stationary frames can use a broader cached
    /// visibility candidate set without rebuilding it for camera-only turns.
    player_moved_last_tick: bool,
    /// True when the latest input frame is manually rotating the camera.
    camera_turning_last_tick: bool,
    /// Index into `MODEL_INSTANCES` for the current lock-on target.
    /// Only live gameplay entities participate; scenery model instances
    /// never enter the combat target set.
    lock_target: Option<usize>,
    lock_switch_stick_held: bool,
    /// Consecutive ticks where the hard-lock target is outside break range.
    /// Dead/despawned targets still release immediately.
    lock_invalid_ticks: u8,
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
    /// Persistent model bytes are resident and the parsed runtime tables are valid.
    runtime_models_loaded: bool,
    /// Last world-ready condition set reported by `trace_world_ready_conditions`,
    /// OR'd with a seen bit so the zeroed initial value cannot be mistaken for
    /// "all conditions false, already reported".
    #[cfg(feature = "boot-trace")]
    world_ready_trace_mask: u32,
    /// Last persistent-asset progress reported, so the trace can show movement.
    #[cfg(feature = "boot-trace")]
    world_ready_trace_progress: i32,
    /// Whether the persistent-asset failure has already been announced. The
    /// flag is sticky and the loading screen runs forever, so without this the
    /// report would repeat every tick.
    #[cfg(feature = "cd-stream-bench")]
    persistent_failure_reported: bool,
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
    /// True while any active room's texture is unresolved (in flight OR
    /// dropped). Drives the material-refresh retry: a DROPPED texture
    /// (upload queue full at build time) never produces an upload
    /// completion, so a completion-gated refresh would leave it on the
    /// untextured fallback forever (the menu-path wall-flicker bug:
    /// the fallback's CLUT sits at VRAM (0,0) inside the framebuffer,
    /// so the surface visibly tracks framebuffer contents).
    room_materials_unresolved: bool,
}

impl Playtest {
    /// Make the zeroed `SCENE` storage a valid `Playtest`, then stamp the
    /// boot-time initial state (everything the old `const fn new()`
    /// initializer stored as non-zero `.data` bytes).
    ///
    /// # Safety
    ///
    /// `scene` must point to `SCENE`'s link-time-zero storage, before any
    /// other access to it: phase 1 below writes `None` into every
    /// `Option` field through raw places because a niched `Option`'s
    /// all-zero bytes can decode as `Some(<invalid payload>)`, and a
    /// `&mut Playtest` may only be minted once every field holds a valid
    /// value. Single-threaded boot path.
    unsafe fn init_zeroed(scene: *mut Self) {
        use core::ptr::addr_of_mut;
        // Phase 1 -- validity: `None` for EVERY `Option` field, so the
        // borrow below stays sound no matter which payload niche the
        // compiler picked for each `None` encoding (empirically, the
        // room/model/font/material `Option`s niche into a payload
        // bool/enum byte, making their `None` a NON-zero byte).
        addr_of_mut!((*scene).room).write(None);
        addr_of_mut!((*scene).bsp).write(None);
        addr_of_mut!((*scene).current_collision_room).write(None);
        addr_of_mut!((*scene).character).write(None);
        addr_of_mut!((*scene).player_actor_pose).write(None);
        addr_of_mut!((*scene).lock_target).write(None);
        addr_of_mut!((*scene).soft_lock_target).write(None);
        addr_of_mut!((*scene).active_interactable).write(None);
        addr_of_mut!((*scene).checkpoint).write(None);
        addr_of_mut!((*scene).message_overlay).write(None);
        addr_of_mut!((*scene).shadow_material).write(None);
        addr_of_mut!((*scene).particle_material).write(None);
        for slot in 0..MAX_RUNTIME_UI_FONTS {
            addr_of_mut!((*scene).ui_fonts[slot]).write(None);
        }
        for slot in 0..MAX_RUNTIME_MODELS {
            addr_of_mut!((*scene).models[slot]).write(None);
        }
        for slot in 0..MAX_RUNTIME_MODEL_CLIPS {
            addr_of_mut!((*scene).clips[slot]).write(None);
        }
        for slot in 0..MAX_MODEL_INSTANCES {
            addr_of_mut!((*scene).instance_actor_poses[slot]).write(None);
        }
        for slot in 0..MAX_ACTIVE_ROOMS {
            addr_of_mut!((*scene).window.rooms[slot]).write(None);
            addr_of_mut!((*scene).window.job.rooms[slot]).write(None);
            addr_of_mut!((*scene).window.job.previous_rooms[slot]).write(None);
        }
        for slot in 0..MAX_COLLISION_ROOMS {
            addr_of_mut!((*scene).camera_collision_rooms[slot].room).write(None);
        }
        // Phase 2 -- boot state: every field is now a valid value, so
        // mint the exclusive borrow and stamp the non-zero initial state.
        (*scene).init_boot_state();
    }

    /// Stamp the non-zero boot state (what the old `const fn new()`
    /// stored in `.data`) onto the zeroed-and-made-valid scene: ambient
    /// light, sentinel-filled pools, camera rig, fallback materials, and
    /// the streaming-job budget. Per-element loops for the pools -- no
    /// whole-array temporaries. Every field not stamped here boots as
    /// all-zero bytes, which IS its old initializer value.
    fn init_boot_state(&mut self) {
        self.current_ambient_rgb = [0x80; 3];
        self.visibility.init();
        #[cfg(all(
            feature = "world-grid-visible",
            not(feature = "vis-full-active-chunks")
        ))]
        self.visible_cells.init();
        for room in self.window.job.requested_rooms.iter_mut() {
            *room = INVALID_ROOM_INDEX;
        }
        for room in self.resident_desired.iter_mut() {
            *room = INVALID_ROOM_INDEX;
        }
        for material in self.materials.iter_mut() {
            *material = room_material_fallback();
        }
        self.motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        // Zero bytes already decode as `Idle`; stamped for self-documentation.
        self.anim_state = PlayerAnim::Idle;
        self.anim_blend_from = None;
        self.box_props.init();
        self.orbit_yaw = CAMERA_START_YAW;
        self.orbit_radius = CAMERA_START_RADIUS;
        self.camera = ThirdPersonCameraState::new(CAMERA_START_YAW);
        self.render_camera = WorldCamera::from_basis(
            PROJECTION,
            WorldVertex::ZERO,
            Q12::ZERO,
            Q12::ONE,
            Q12::ZERO,
            Q12::ONE,
        );
        self.overlay_camera = WorldCamera::from_basis(
            PROJECTION,
            WorldVertex::ZERO,
            Q12::ZERO,
            Q12::ONE,
            Q12::ZERO,
            Q12::ONE,
        );
        self.prepared_overlay_camera = self.overlay_camera;
        self.camera_rooms_key = (INVALID_ROOM_INDEX, i32::MIN, i32::MIN, 0, 0);
        for vertex in self.model_vertices.iter_mut() {
            *vertex = ModelVertex::ZERO;
        }
        self.streaming_jobs = RuntimeStreamingJobs::new();
        self.room_materials_unresolved = true;
        self.bsp_instance_visible_mask = u16::MAX;
    }

    fn step_streaming_jobs(&mut self, ctx: &mut Ctx) {
        let background_tick = self.streaming_jobs.background_tick(ctx);
        // Every tick, not just background ticks. The controller steps over
        // sectors that land while nobody is collecting, and this read is a
        // loading screen with nothing to stay responsive for.
        #[cfg(feature = "cd-stream-bench")]
        if !self.step_persistent_model_assets() {
            // The model pack and WORLD.PAK share one physical CD controller.
            // Finish the session-lifetime read before room residency can seek.
            return;
        }
        // Resident PXBSP owns world geometry, PVS, and collision. It has no
        // synthetic PSXW room stream/window; only shared VRAM uploads and BSP
        // material resolution remain after persistent models are ready.
        if self.bsp.is_some() {
            if background_tick {
                let _ = self.streaming_jobs.step_vram_uploads();
                if let Some(bsp) = self.bsp.as_mut() {
                    let _ = bsp.refresh_materials();
                }
            }
            return;
        }
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
            // Stream progress marks the window dirty: newly resident
            // rooms can unblock builds the last pass skipped.
            #[cfg(feature = "cd-stream-bench")]
            if stream_progress {
                self.active_window_dirty = true;
            }
            // Pump the material refresh while any room texture is unresolved, not
            // only when an upload completes. A dropped texture (queue was full) is
            // never queued, so it produces no completion to wake a refresh; without
            // the unresolved retry it stays the untextured fallback forever (and
            // the fallback CLUT lives inside the framebuffer, so the surface
            // visibly tracks stale frame contents -- the menu-path wall flicker).
            let upload_completed = self.streaming_jobs.step_vram_uploads();
            if upload_completed || self.room_materials_unresolved {
                self.room_materials_unresolved = self.refresh_active_room_materials();
            }
            self.reconcile_active_room_window();
        }
    }

    fn initial_world_ready(&mut self) -> bool {
        #[cfg(not(feature = "cd-stream-bench"))]
        {
            self.bsp.as_ref().is_none_or(BspRuntime::materials_ready)
        }
        #[cfg(feature = "cd-stream-bench")]
        {
            if !self.runtime_models_loaded {
                // Traced too: this is the earliest condition and it returns
                // before the rest are even evaluated, so a stall here is
                // otherwise silent.
                // `failed` is sticky and stops the pump, so a single bad
                // persistent asset hangs the loading screen forever with
                // nothing reported. Distinguish the two.
                let arena_failed = persistent_assets_arena().failed();
                if arena_failed && !self.persistent_failure_reported {
                    // Unconditional, not behind `boot-trace`: this state never
                    // resolves, so the run is over and the only question left
                    // is which asset and why. Reason 0..11 is a cd_stream chunk
                    // status, 100+ an asset_streaming reason code.
                    self.persistent_failure_reported = true;
                    psx_rt::tty::print("PERSISTENT ASSET LOAD FAILED: asset ");
                    psx_rt::tty::print_hex_u32(persistent_assets_arena().failed_asset() as u32);
                    psx_rt::tty::print(" reason ");
                    psx_rt::tty::print_hex_u32(persistent_assets_arena().failed_reason());
                    psx_rt::tty::println("");
                }
                #[cfg(feature = "boot-trace")]
                {
                    // Distinguish "slow" from "wedged": a load that is merely
                    // slow keeps moving this.
                    let progress = persistent_assets_arena().progress_q12();
                    if progress != self.world_ready_trace_progress {
                        self.world_ready_trace_progress = progress;
                        psx_rt::tty::print("persistent progress q12 ");
                        psx_rt::tty::print_hex_u32(progress as u32);
                        psx_rt::tty::println("");
                    }
                }
                self.trace_world_ready_conditions(&[
                    ("runtime_models_loaded", false),
                    ("persistent_assets_not_failed", !arena_failed),
                ]);
                return false;
            }
            let bsp_ready = self.bsp.as_ref().is_none_or(BspRuntime::materials_ready);
            if !self.chunked_level() {
                return bsp_ready;
            }
            let Some(record) = ROOMS.get(self.room_index.to_usize()) else {
                return true;
            };
            let textures_ready = self.initial_stream_ring_textures_ready();
            let conditions = [
                ("collision_room", self.current_collision_room.is_some()),
                ("window_job_idle", !self.window.job.active),
                (
                    "portal_rooms_active",
                    self.portal_visible_rooms_are_active(record),
                ),
                ("ring_resident", self.initial_stream_ring_resident()),
                ("ring_textures", textures_ready),
                ("bsp_textures", bsp_ready),
                ("vram_uploads_idle", self.streaming_jobs.vram_uploads_idle()),
                ("stream_quiet", !streamed_room_stream_active()),
            ];
            self.trace_world_ready_conditions(&conditions);
            let mut ready = true;
            let mut i = 0;
            while i < conditions.len() {
                ready &= conditions[i].1;
                i += 1;
            }
            ready
        }
    }

    /// Report which world-ready conditions are still false, once per change.
    ///
    /// A stalled load is otherwise invisible: `initial_world_ready` collapses
    /// seven conditions into one bool, none of them is a telemetry counter, and
    /// the loading screen looks identical whichever one is stuck. Finding that
    /// out cost a whole session once (see docs/known-issues-2026-07-25.md).
    ///
    /// Printing only on change keeps a 30 Hz loading loop from flooding the TTY
    /// while still showing the order conditions settle in, which is what says
    /// whether a load is progressing or wedged.
    #[cfg(feature = "cd-stream-bench")]
    fn trace_world_ready_conditions(&mut self, conditions: &[(&str, bool)]) {
        #[cfg(not(feature = "boot-trace"))]
        {
            let _ = conditions;
        }
        #[cfg(feature = "boot-trace")]
        {
            let mut mask = 0u32;
            let mut i = 0;
            while i < conditions.len() {
                if conditions[i].1 {
                    mask |= 1 << i;
                }
                i += 1;
            }
            const TRACED: u32 = 1 << 31;
            if mask | TRACED == self.world_ready_trace_mask {
                return;
            }
            self.world_ready_trace_mask = mask | TRACED;
            psx_rt::tty::println("world-ready pending:");
            let mut i = 0;
            while i < conditions.len() {
                if !conditions[i].1 {
                    psx_rt::tty::println(conditions[i].0);
                }
                i += 1;
            }
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
            let room = self.visibility.result.rooms[visible].room;
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
                ready &= room_reflection_probe_ready(room);
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
            let room = self.visibility.result.rooms[visible].room;
            if room != INVALID_ROOM_INDEX && !room_requested(room, &self.resident_desired, count) {
                if let Some(record) = ROOMS.get(room.to_usize()) {
                    ready &= room_material_textures_ready(record);
                    ready &= room_backdrop_textures_ready(record);
                }
                ready &= room_reflection_probe_ready(room);
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

fn playtest_visual_pacing(video_mode: VideoMode) -> VisualPacing {
    match video_mode {
        VideoMode::Ntsc => VisualPacing::EveryNVBlanks(2),
        // PAL is 50Hz, so exact 30Hz pacing does not divide cleanly.
        // Use a deterministic 25Hz fallback instead of a jittery cadence.
        VideoMode::Pal => VisualPacing::EveryNVBlanks(2),
    }
}

/// The `Playtest` scene is ~227 KB; keeping it as a `main` stack local
/// pushed `$sp` down into `.bss`. Place it in static storage so it no
/// longer competes with the stack for the same region.
///
/// Flat-binary discipline (same as `runtime_arenas`): the storage must be
/// link-time zero so it lands in `.bss` -- NOLOAD in the PSX-EXE -- instead
/// of storing ~227 KB of initializer bytes in the flat binary's `.data`
/// (which the old `Playtest::new()` image did: sentinel-filled pools,
/// fallback materials, and niched `Option` fields are non-zero bytes).
///
/// `MaybeUninit` storage rather than an all-zero `const Playtest` because
/// no such value exists: several `Option` fields niche their discriminant
/// into a payload bool/enum byte, so their `None` is a NON-zero byte and
/// the all-zero pattern would decode as `Some(<invalid payload>)`. NOT
/// ready for use until [`Playtest::init_zeroed`] stamps validity and the
/// boot state at the top of `main`, before anything else touches it.
static mut SCENE: core::mem::MaybeUninit<Playtest> = core::mem::MaybeUninit::zeroed();

#[no_mangle]
fn main() -> ! {
    // Stamp the scene's boot state onto its link-time-zero storage BEFORE
    // any other use (see `SCENE` and `Playtest::init_zeroed`).
    // SAFETY: single-threaded boot path; the raw stamping happens before
    // this one exclusive borrow is minted (`MaybeUninit<T>` is
    // `#[repr(transparent)]`, so the cast is layout-exact), and nothing
    // else touches the static.
    let scene: &mut Playtest = unsafe {
        let scene = core::ptr::addr_of_mut!(SCENE).cast::<Playtest>();
        Playtest::init_zeroed(scene);
        &mut *scene
    };
    // The scene now lives in `.bss`, not on the stack; this guard still
    // checks that the live stack frames (and the rest of static data) leave
    // headroom between `$sp` and the top of `.bss` instead of silently
    // corrupting `static` buffers at runtime.
    #[cfg(target_arch = "mips")]
    psx_rt::assert_stack_headroom();
    // Stamp the runtime arenas' initial state onto their link-time-zero
    // storage before anything can touch them (see `runtime_arenas`).
    init_runtime_arenas();
    game_trace("editor-playtest: init ok");
    let video_mode = VideoMode::Ntsc;
    let config = Config {
        clear_color: (5, 7, 12),
        video_mode,
        visual_pacing: playtest_visual_pacing(video_mode),
        scheduler: SchedulerConfig::new()
            .with_max_fixed_ticks_before_visual(RUNTIME_SCHEDULE.max_fixed_ticks_before_visual)
            .with_visual_lockstep(cfg!(feature = "lockstep-visuals")),
        loading_ui_scene: generated::LOADING_UI_SCENE,
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
        scene,
    );
}
