// SPDX-License-Identifier: GPL-2.0-or-later
//! `psx-engine` -- PSoXide engine layer.
//!
//! The SDK exposes the PS1's hardware surface (GPU, SPU, GTE, pad
//! VRAM layout, primitives). The engine sits one level up and
//! provides the things a *game* actually wants:
//!
//! - a [`Scene`] trait and an [`App::run`] entry point so games
//!   don't each reinvent the main loop;
//! - a [`Ctx`] carrying per-frame state (pad, simulation tick,
//!   visual frame counter, framebuffer) to the scene;
//! - a canonical [`Angle`] unit so we stop hitting the recurring
//!   "256-per-revolution vs 4096-per-revolution" angle-mismatch bug
//!   that cost an afternoon on showcase-fog's light orbit;
//! - typed coordinate-space wrappers like [`RoomPoint`] so gameplay
//!   room-local positions do not quietly mix with raw renderer
//!   submission vertices;
//! - typed fixed-point scalars like [`Q12`] and [`Q8`] so movement,
//!   light intensity, and falloff code can name its unit scale;
//! - render helpers for ordering-table frames and fixed primitive
//!   arenas, so games can build PS1 painter's-algorithm command
//!   streams without rewriting OT ceremony in every scene.
//!
//! The engine is `no_std`, has no allocator dependency, and compiles
//! only for `target_arch = "mips"` (host stubs mirror the SDK's
//! pattern so `cargo check` still works on the host). Nothing here
//! touches disc / asset streaming -- that's the game's or the
//! content-pipeline's concern.
//!
//! # Minimal usage
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! extern crate psx_rt;
//!
//! use psx_engine::{App, Config, Ctx, Scene};
//!
//! struct Game;
//!
//! impl Scene for Game {
//!     fn update(&mut self, _ctx: &mut Ctx) {}
//!     fn render(&mut self, _ctx: &mut Ctx) {}
//! }
//!
//! #[no_mangle]
//! fn main() -> ! {
//!     App::run(Config::default(), &mut Game);
//! }
//! ```

#![no_std]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod affine_surface;
pub mod angle;
pub mod app;
pub mod attributed_clip;
pub mod cd_drive;
pub mod character_motor;
pub mod collision_query;
pub mod fixed;
pub mod frames;
pub mod game_app;
pub mod lighting;
pub mod microgame;
pub mod movement;
mod present_queue;
pub mod projection;
pub mod render;
pub mod render3d;
pub mod scene;
pub mod scheduler;
pub mod scratch;
pub mod scratchpad;
pub mod sfx;
pub mod telemetry;
pub mod tess;
pub mod third_person_camera;
mod time;
pub mod transform;
mod transitions;
pub mod ui;
pub mod world_render;

/// Compare two `usize` values without exposing an R3000A load-delay hazard.
///
/// LLVM can place an operand reload in a taken branch's delay slot and consume
/// the stale register in the target block. Keeping the delay instruction and
/// comparison in one asm block forces both operands to be loaded before the
/// `nop`, so the following `sltu` sees their current values. Host builds use
/// the ordinary comparison.
#[inline(always)]
pub(crate) fn r3000_usize_gt(left: usize, right: usize) -> bool {
    #[cfg(target_arch = "mips")]
    {
        let result: usize;
        unsafe {
            core::arch::asm!(
                ".set push",
                ".set noat",
                "nop",
                "sltu {result}, {right}, {left}",
                ".set pop",
                left = in(reg) left,
                right = in(reg) right,
                result = lateout(reg) result,
                options(nomem, nostack, preserves_flags)
            );
        }
        result != 0
    }
    #[cfg(not(target_arch = "mips"))]
    {
        left > right
    }
}

pub use affine_surface::{
    compose_model_view_transform, materialize_baked_surface_vertices, materialize_surface_vertices,
    submit_surface_batch, submit_surface_batch_near, submit_surface_batch_screened, AffineSurface,
    AffineVertex, NearView, SurfaceProfile, SurfaceSourceVertex, SurfaceSubmit,
    AFFINE_PACKETS_PER_TRIANGLE, AFFINE_SPLIT_SCRATCH_VERTICES,
};
pub use angle::Angle;
pub use app::{App, Config, VisualPacing};
pub use character_motor::{
    commit_body_direction_with_trace_provider, commit_body_step_with_trace_provider, BodyStep,
    CharacterBlockerTraceProvider, CharacterCollisionAabb, CharacterCollisionCylinder,
    CharacterMotorAction, CharacterMotorAnim, CharacterMotorConfig, CharacterMotorFrame,
    CharacterMotorInput, CharacterMotorState,
};

pub use collision_query::{
    trace_collision, CollisionQueryError, CollisionTrace, CollisionTraceProvider,
    CollisionTraceQuery, CollisionTraceShape, COLLISION_FRACTION_ONE_Q12,
};
pub use fixed::{div_q12_i32, Q12, Q8};
pub use frames::{Frames, SimTick, Ticks, VideoHz, VisualFrame};
pub use game_app::{FlowCursor, GameApp, GAMEPLAY_ONLY};
pub use lighting::{
    accumulate_point_lights, accumulate_point_lights_rgb, modulate_material_tint, modulate_tint,
    shade_material_tint_with_lights, shade_tint_with_lights, LightingRgb, MaterialTint,
    PointLightSample, Rgb8, LIGHTING_MAX, LIGHTING_NEUTRAL,
};
pub use microgame::{MicrogameAction, MicrogameScreen, MicrogameShell};
pub use movement::{
    camera_relative_move, camera_relative_move_axes, camera_relative_move_q12,
    horizontal_view_coordinates, yaw_to_point, CameraRelativeMove, InputAxis, InputAxisProfile,
    InputVector,
};
/// The GPU DMA ownership token [`OtFrame::submit`] takes; scenes borrow
/// the app runner's through [`Ctx::gpu_dma`].
pub use psx_io::periph::GpuDma;
pub use render::{
    CameraDepth, DepthBand, DepthRange, DepthSlot, GpuPacket, OtDepth, OtFrame, PacketFramePair,
    PrimitiveArena, PrimitivePacketArena, PrimitivePacketScratch, PrimitivePacketStream,
    PrimitivePacketWordReservation, PrimitiveSink, RoomSurfaceSink, PRIMITIVE_PACKET_SLOT_WORDS,
};
pub use render3d::{
    apply_model_pose_translation, compute_joint_view_transform, compute_joint_world_basis,
    compute_joint_world_transform, project_model_vertex_with_joint_transforms,
    projected_model_face_batchable, projected_triangle_batchable, AdaptiveSubdivisionKindMask,
    AdaptiveSubdivisionProfile, CullMode, DepthPolicy, GouraudMeshOptions, GouraudRenderPass,
    GouraudTriCommand, JointViewTransform, JointWorldTransform, LoadedAnchoredCameraGte,
    LoadedWorldCameraGte, LocalToWorldScale, MeshRenderStats, ModelPoseTranslation, ModelUvMapping,
    ModelUvOffset, PredecodedModelInfo, ProjectedLit, ProjectedTexturedVertex, ProjectedVertex,
    SkyDirectionProjector, TexturedModelGeometry, TexturedModelLayer, TexturedModelRenderFace,
    TexturedModelRenderStats, TexturedViewVertex, ViewVertex, WorldCamera, WorldProjection,
    WorldRenderLayer, WorldRenderPass, WorldRenderStats, WorldSurfaceOptions, WorldTriCommand,
};
pub use scheduler::{
    collect_due_tasks, FixedUpdateOutcome, FrameScheduler, OverloadPolicy, SchedulerAction,
    SchedulerConfig, TaskBudget, TaskCadence, TaskDescriptor, TaskId, TaskLane, TASK_FIXED_UPDATE,
    TASK_VISUAL_RENDER,
};
pub use scratch::{BoundedSink, FixedScratch, SliceSink};
// Re-export the GTE math types callers need to construct model render
// arguments (instance rotation, joint transforms) without pulling in
// `psx-gte` directly.
pub use psx_gte::math::{Mat3I16, Vec3I16};
pub use scene::{Ctx, QueuedFrame, RenderSubmission, Scene, SceneStateRef};
pub use third_person_camera::{
    accelerated_orbit_step_q12, ThirdPersonCameraConfig, ThirdPersonCameraFrame,
    ThirdPersonCameraInput, ThirdPersonCameraProfile, ThirdPersonCameraState,
    ThirdPersonCameraTarget,
};
pub use transform::{ActorTransform, RoomPoint, Vec3World, WorldVertex};
pub use ui::{draw_scene, is_focusable, node_nav_rect, UiTextureSlot, UI_CANVAS_H, UI_CANVAS_W};
pub use world_render::{
    NoWorldSurfaceLighting, SurfaceSidedness, WorldMaterialAnimation, WorldRenderMaterial,
    WorldSurfaceKind, WorldSurfaceLighting, WorldSurfaceSample,
};

/// Button-mask constants (UP, DOWN, CROSS, START, …) re-exported
/// from `psx_pad::button` so games using `Ctx::just_pressed` /
/// `is_held` don't need a direct `psx-pad` dep just for the button
/// names.
pub use psx_pad::button;
/// Pad-state types re-exported for scenes that need analog stick data.
pub use psx_pad::{
    ActionBinding, ActionInput, ActionMap, AnalogSticks, Deadzone, PadMode, PadState, STICK_FULL,
};

/// The controller-port token behind [`Ctx::controller_port`], for scenes that
/// drive a memory card or negotiate a pad mode without a direct `psx-io` dep.
pub use psx_io::periph::ControllerPort;

mod masked_pose;
pub use masked_pose::MaskedPoseBlend;
