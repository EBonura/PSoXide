//! Fixed-budget third-person camera controller.
//!
//! The controller is designed for PS1-scale rooms: no heap, no dynamic
//! dispatch, bounded ray work, integer math, and collision probes that
//! read the cooked grid room through [`RoomCollision`]. It supplies the
//! common action-camera pieces a game wants on top of [`WorldCamera`]:
//! manual orbit cooldown, optional automatic re-alignment, camera lag,
//! lock-on facing, and a spring-arm collision solve that shortens the
//! boom without taking yaw control away from the player.

mod framing;
mod profile;
use framing::lock_pitch_goal;
use profile::ProfileBlend;
pub use profile::ThirdPersonCameraProfile;

use crate::floor_sample::{height_at_local, triangle_heights_to_quad};
use crate::{
    collision_query::{
        trace_collision, CollisionQueryError, CollisionTraceProvider, CollisionTraceQuery,
        COLLISION_FRACTION_ONE_Q12,
    },
    fixed::div_q12_i32,
    Angle, CharacterCollisionRoom, RoomCollision, RoomPoint, WorldCamera, WorldProjection, Q12,
};
use psx_math::int32::{abs_i16, abs_i32, isqrt_i32, mul_q12_i32};

const RAY_STEPS_MAX: i32 = 8;
const RAY_STEPS_MIN: i32 = 3;
const RAY_NEIGHBORHOOD_CELLS: usize = 9;
const MAX_RAY_CHECKED_CELLS: usize = RAY_STEPS_MAX as usize * RAY_NEIGHBORHOOD_CELLS;
const CHECKED_CAMERA_CELL_BITS: usize = 512;
const CHECKED_CAMERA_CELL_WORDS: usize = CHECKED_CAMERA_CELL_BITS / 32;
const MAX_CAMERA_COLLISION_ROOMS: usize = 4;
const MAX_CAMERA_CATCHUP_VBLANKS: u16 = 4;
const TRACE_CAMERA_FLOOR_PROBE_DOWN: i32 = 32_767;
const TRACE_CAMERA_FLOOR_PROBE_LIFT: i32 = 1;

// Mirrors psxed_format::world::direction::* without adding a direct
// psxed-format dependency just for byte constants.
const DIR_NORTH: u8 = 0;
const DIR_EAST: u8 = 1;
const DIR_SOUTH: u8 = 2;
const DIR_WEST: u8 = 3;
const DIR_NORTH_WEST_SOUTH_EAST: u8 = 4;
const DIR_NORTH_EAST_SOUTH_WEST: u8 = 5;

/// Tunables for [`ThirdPersonCameraState`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThirdPersonCameraConfig {
    /// Preferred trailing distance from focus to camera.
    pub distance: i32,
    /// Preferred closest camera distance when unobstructed. A blocking surface
    /// closer than this still wins so the camera never crosses into solid space.
    pub min_distance: i32,
    /// Furthest distance the camera may ease back out to.
    pub max_distance: i32,
    /// Vertical camera offset above the player origin.
    pub height: i32,
    /// Vertical look-at offset above the player origin.
    pub target_height: i32,
    /// Lateral composition offset. Positive moves the camera to screen-right.
    pub shoulder_offset: i32,
    /// Additional vertical camera lift while a target is locked.
    /// The focus remains anchored to the player so this cannot push the
    /// character out through the bottom of the viewport.
    pub lock_height_boost: i32,
    /// Minimum camera origin height above the sampled floor.
    pub min_floor_clearance: i32,
    /// Extra clearance kept between the camera ray and blocking geometry.
    pub collision_margin: i32,
    /// Lowest manual pitch, in signed Q0.12 turn units.
    pub pitch_min_q12: i16,
    /// Highest manual pitch, in signed Q0.12 turn units.
    pub pitch_max_q12: i16,
    /// Display frames before auto-alignment resumes after manual camera input.
    pub manual_cooldown_frames: u8,
    /// Fade automatic alignment back in over this many display ticks after cooldown.
    pub manual_release_frames: u8,
    /// Optional accelerated orbit speed level (1-7). Input deltas must use
    /// `accelerated_orbit_step_q12` as their full-stick rate.
    pub accelerated_orbit_speed: Option<u8>,
    /// Keep the chosen orbit pitch during recenter, completing yaw in 18 ticks.
    pub recenter_preserves_pitch: bool,
    /// Vertical FOV, clamped to 38-48 degrees. Zero keeps the supplied projection.
    pub fov_y_degrees: u8,
    /// Ease profile values at 60 Hz, with a second distance approach stage.
    pub blend_profiles: bool,
    /// Use the elevated lock anchor to steer pitch around the player focus.
    /// Replaces legacy target bias and lock height boost.
    pub lock_target_framing: bool,
    /// Optional composition while a lock target is present.
    pub lock_profile: Option<ThirdPersonCameraProfile>,
    /// Temporary composition (for example held weapon aiming), independent of lock.
    pub composition_override: Option<ThirdPersonCameraProfile>,
    /// Per-tick profile response in Q12; 104 preserves the ordinary follow blend.
    pub profile_response_q12: u16,
    /// Preserve viewing pitch when a temporary composition changes boom length.
    pub preserve_profile_pitch: bool,
    /// Target's vertical framing above centre, as percent of the half-FOV.
    pub lock_frame_percent: u8,
    /// Maximum auto-align yaw movement per display frame.
    pub auto_align_step: Angle,
    /// When true, ease the unlocked camera behind player yaw while moving.
    pub auto_align_when_moving: bool,
    /// Maximum lock-on yaw movement per display frame.
    pub lock_on_align_step: Angle,
    /// Position lag strength as a power-of-two divisor.
    pub position_lag_shift: u8,
    /// Vertical position lag override. None uses `position_lag_shift`.
    pub position_vertical_lag_shift: Option<u8>,
    /// Focus lag strength as a power-of-two divisor.
    pub focus_lag_shift: u8,
    /// Vertical focus lag override. None uses `focus_lag_shift`. A slower
    /// vertical setting limits focus lag to a quarter of `target_height`
    /// so sharp height changes cannot leave the look-at point above the actor.
    pub focus_vertical_lag_shift: Option<u8>,
    /// Ease-out strength when collision lets the camera extend again.
    pub distance_lag_shift: u8,
    /// Display frames to hold the shortened boom before easing out.
    pub collision_release_delay_frames: u8,
    /// Run the spring-arm collision sweep every Nth display tick and
    /// reuse the previous solve in between (1 = every tick). Distance
    /// easing, pull-in snapping, and yaw/focus lag still run every
    /// tick; manual orbit input, lock-on, and recenter force a fresh
    /// solve so stale collision never fights deliberate camera moves.
    /// Worst-case collision reaction latency grows by (N-1) ticks.
    pub collision_solve_interval: u8,
}

impl ThirdPersonCameraConfig {
    /// Build a camera config from the authored Character camera fields.
    pub const fn character(distance: i32, height: i32, target_height: i32) -> Self {
        Self {
            distance,
            min_distance: 24,
            max_distance: distance,
            height,
            target_height,
            shoulder_offset: 0,
            lock_height_boost: if height > 0 {
                height.saturating_mul(25) / 100
            } else {
                0
            },
            min_floor_clearance: 0,
            collision_margin: 10,
            pitch_min_q12: -192,
            pitch_max_q12: 704,
            manual_cooldown_frames: 42,
            manual_release_frames: 0,
            accelerated_orbit_speed: None,
            recenter_preserves_pitch: false,
            fov_y_degrees: 0,
            blend_profiles: false,
            lock_target_framing: false,
            lock_profile: None,
            composition_override: None,
            profile_response_q12: 104,
            preserve_profile_pitch: false,
            lock_frame_percent: 45,
            auto_align_step: Angle::from_q12(18),
            auto_align_when_moving: false,
            lock_on_align_step: Angle::from_q12(64),
            position_lag_shift: 2,
            position_vertical_lag_shift: None,
            focus_lag_shift: 2,
            focus_vertical_lag_shift: None,
            distance_lag_shift: 3,
            collision_release_delay_frames: 4,
            collision_solve_interval: 1,
        }
    }
}

/// Full-stick angular rate per 60 Hz display tick for the accelerated orbit mode.
/// A squared speed option interpolates 80-240 degrees/second (normal) or
/// 180-500 (fast). Level is clamped to the editor's supported 1-7 range.
/// Rounding is to the nearest Q0.12 turn unit; both axes use the same rate.
pub fn accelerated_orbit_step_q12(speed_level: u8, fast: bool) -> i16 {
    let level = i32::from(speed_level.clamp(1, 7));
    let (base, span) = if fast { (180, 320) } else { (80, 160) };
    (((base * 100 + span * level * level) * 4096 + 1_080_000) / 2_160_000) as i16
}

// Each axis has its own signed hold timer. Release/reversal clears fractional
// carry too, so old motion cannot leak into a fresh gesture. Input is already
// deadzone-scaled; keep its magnitude while changing the maximum angular rate.
fn accelerated_orbit_delta(
    delta: i16,
    speed: Option<u8>,
    ramp: &mut i8,
    remainder: &mut i32,
) -> i16 {
    let Some(speed) = speed else {
        *ramp = 0;
        *remainder = 0;
        return delta;
    };
    if delta == 0 || delta.signum() != i16::from(ramp.signum()) {
        *ramp = 0;
        *remainder = 0;
    }
    if delta == 0 {
        return 0;
    }
    let held = i32::from(ramp.unsigned_abs()).min(36);
    let normal = i32::from(accelerated_orbit_step_q12(speed, false));
    let fast = i32::from(accelerated_orbit_step_q12(speed, true));
    let divisor = normal * 36;
    let numerator = i32::from(delta) * (normal * 36 + (fast - normal) * held) + *remainder;
    let step = numerator / divisor;
    *remainder = numerator % divisor;
    *ramp = (held + 1).min(36) as i8 * delta.signum() as i8;
    step.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// Per-display-frame camera input.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThirdPersonCameraInput {
    /// Signed manual yaw delta in Q0.12 angle units.
    pub yaw_delta_q12: i16,
    /// Signed manual pitch delta in Q0.12 angle units.
    /// Positive raises the camera above the focus point.
    pub pitch_delta_q12: i16,
    /// When true, force the camera to begin easing back behind the player.
    pub recenter: bool,
}

/// Player and optional lock-on target data consumed by the camera.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThirdPersonCameraTarget {
    /// Player/root position in room-local world units.
    pub player: RoomPoint,
    /// Player facing yaw.
    pub player_yaw: Angle,
    /// True while the player is intentionally moving.
    pub moving: bool,
    /// Optional lock-on target position in room-local world units.
    pub lock_target: Option<RoomPoint>,
}

/// Camera solve result for the current frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThirdPersonCameraFrame {
    /// Render camera ready for world/model draw calls.
    pub camera: WorldCamera,
    /// Lagged focus point used by the camera.
    pub focus: RoomPoint,
    /// Camera orbit yaw.
    pub yaw: Angle,
    /// Camera pitch, signed Q0.12 turn units.
    pub pitch_q12: i16,
    /// Current camera distance after collision.
    pub distance: i32,
    /// True when the camera was shortened by collision this frame.
    pub collision_pull_in: bool,
    /// Reserved for older debug overlays; spring-arm collision no
    /// longer steers yaw, so this is currently always false.
    pub collision_rotated: bool,
}

/// Runtime state for the third-person camera.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThirdPersonCameraState {
    profile: ProfileBlend,
    yaw: Angle,
    pitch_q12: i16,
    frame_pitch_q12: i16,
    lock_height_offset: i32,
    lock_pitch_offset_q12: i16,
    base_position_y: i32,
    distance: i32,
    position: RoomPoint,
    focus: RoomPoint,
    manual_cooldown: u8,
    manual_release: u8,
    yaw_ramp: i8,
    pitch_ramp: i8,
    yaw_remainder: i32,
    pitch_remainder: i32,
    recenter_remaining: u8,
    recenter_active: bool,
    collision_release_delay: u8,
    initialized: bool,
    last_pull_in: bool,
    last_rotated: bool,
    solve_phase: u8,
    cached_solve: CollisionSolve,
    /// The orbit was swung off a wall by [`clear_orbit_yaw`]; lock-on and
    /// recentering hold that yaw until the direction they steer to is clear.
    clear_orbit_hold: bool,
}

impl ThirdPersonCameraState {
    /// Create a camera state with an initial orbit yaw.
    pub const fn new(yaw: Angle) -> Self {
        Self {
            profile: ProfileBlend::new(),
            yaw,
            pitch_q12: 0,
            frame_pitch_q12: 0,
            lock_height_offset: 0,
            lock_pitch_offset_q12: 0,
            base_position_y: 0,
            distance: 0,
            position: RoomPoint::ZERO,
            focus: RoomPoint::ZERO,
            manual_cooldown: 0,
            manual_release: 0,
            yaw_ramp: 0,
            pitch_ramp: 0,
            yaw_remainder: 0,
            pitch_remainder: 0,
            recenter_remaining: 0,
            recenter_active: false,
            collision_release_delay: 0,
            initialized: false,
            last_pull_in: false,
            last_rotated: false,
            solve_phase: 0,
            cached_solve: CollisionSolve {
                distance: 0,
                pull_in: false,
            },
            clear_orbit_hold: false,
        }
    }

    /// Reset the camera immediately behind a player position.
    pub fn snap_to_player(
        &mut self,
        target: ThirdPersonCameraTarget,
        config: ThirdPersonCameraConfig,
    ) {
        self.snap_to_player_with_yaw(target, config, target.player_yaw.add(Angle::HALF));
    }

    /// Reset the camera around a player position using an explicit
    /// orbit yaw. Useful for editor/playtest starts where the
    /// authored player yaw should affect the model facing without
    /// the camera immediately hiding that rotation by moving behind
    /// the player.
    pub fn snap_to_player_with_yaw(
        &mut self,
        target: ThirdPersonCameraTarget,
        config: ThirdPersonCameraConfig,
        yaw: Angle,
    ) {
        let config = normalize_config(config);
        self.profile.snap(config, target.lock_target.is_some());
        let config = self.profile.apply(config);
        self.yaw = yaw;
        self.recenter_active = false;
        self.distance = config
            .distance
            .clamp(config.min_distance, config.max_distance);
        self.pitch_q12 = default_pitch_q12(config);
        self.frame_pitch_q12 = self.pitch_q12;
        self.lock_height_offset = 0;
        self.lock_pitch_offset_q12 = 0;
        self.focus = player_focus(target.player, config.target_height);
        self.base_position_y = camera_height_goal(target.player, self.pitch_q12, config);
        self.position = camera_position_at_height(
            self.focus,
            self.distance,
            self.yaw,
            self.pitch_q12,
            self.base_position_y,
        );
        self.manual_cooldown = 0;
        self.manual_release = 0;
        self.yaw_ramp = 0;
        self.pitch_ramp = 0;
        self.yaw_remainder = 0;
        self.pitch_remainder = 0;
        self.recenter_remaining = 0;
        self.collision_release_delay = 0;
        self.clear_orbit_hold = false;
        self.initialized = true;
        self.last_pull_in = false;
        self.last_rotated = false;
        self.solve_phase = 0;
        self.cached_solve = CollisionSolve {
            distance: self.distance,
            pull_in: false,
        };
    }

    /// Re-express the camera in a different room-local coordinate
    /// space while preserving the same physical camera/focus
    /// positions. Streaming chunk transitions should call this with
    /// the same local-space delta applied to the player root.
    pub fn relocate_room_space(&mut self, delta: RoomPoint) {
        self.position = RoomPoint::new(
            self.position.x.saturating_add(delta.x),
            self.position.y.saturating_add(delta.y),
            self.position.z.saturating_add(delta.z),
        );
        self.focus = RoomPoint::new(
            self.focus.x.saturating_add(delta.x),
            self.focus.y.saturating_add(delta.y),
            self.focus.z.saturating_add(delta.z),
        );
        self.base_position_y = self.base_position_y.saturating_add(delta.y);
    }

    /// Advance the controller by one display tick and build a render camera.
    pub fn update(
        &mut self,
        projection: WorldProjection,
        collision: Option<RoomCollision<'_, '_>>,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
    ) -> ThirdPersonCameraFrame {
        self.update_vblanks(projection, collision, target, input, config, 1)
    }

    /// Advance the controller by elapsed display ticks and build a render camera.
    ///
    /// Heavy render paths can miss VBlanks. The camera catches up
    /// with bounded fixed substeps so yaw limits, cooldowns, easing,
    /// and collision recovery keep their authored display-time speed.
    pub fn update_vblanks(
        &mut self,
        projection: WorldProjection,
        collision: Option<RoomCollision<'_, '_>>,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
        delta_vblanks: u16,
    ) -> ThirdPersonCameraFrame {
        let mut collision = GridCameraCollision {
            collision: CameraCollision::Single(collision),
        };
        match self.update_vblanks_with_backend(
            projection,
            &mut collision,
            target,
            input,
            config,
            delta_vblanks,
        ) {
            Ok(frame) => frame,
            Err(_) => unreachable!("grid camera collision queries are infallible"),
        }
    }

    /// Advance the controller against a fixed active-room collision set.
    ///
    /// Chunked levels keep the player, camera, and focus in the current room's
    /// local coordinate space. Nearby chunks are supplied with offsets into
    /// that same space, mirroring the character motor's multi-room collision
    /// path so the spring arm can cross loaded chunk boundaries and still hit
    /// walls.
    pub fn update_vblanks_with_collision_rooms(
        &mut self,
        projection: WorldProjection,
        collision_rooms: &[CharacterCollisionRoom<'_>],
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
        delta_vblanks: u16,
    ) -> ThirdPersonCameraFrame {
        let mut collision = GridCameraCollision {
            collision: CameraCollision::Rooms(collision_rooms),
        };
        match self.update_vblanks_with_backend(
            projection,
            &mut collision,
            target,
            input,
            config,
            delta_vblanks,
        ) {
            Ok(frame) => frame,
            Err(_) => unreachable!("grid camera collision queries are infallible"),
        }
    }

    /// Advance the camera through an allocation-free point-trace provider.
    ///
    /// The spring arm and floor-clearance probe share the provider's
    /// caller-owned scratch. Provider failure restores the complete controller
    /// state and returns an error instead of treating malformed world data as
    /// either a clear path or an occluder.
    pub fn update_vblanks_with_trace_provider<P: CollisionTraceProvider + ?Sized>(
        &mut self,
        projection: WorldProjection,
        provider: &mut P,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
        delta_vblanks: u16,
    ) -> Result<ThirdPersonCameraFrame, CollisionQueryError> {
        let saved = *self;
        let mut collision = TraceCameraCollision { provider };
        match self.update_vblanks_with_backend(
            projection,
            &mut collision,
            target,
            input,
            config,
            delta_vblanks,
        ) {
            Ok(frame) => Ok(frame),
            Err(error) => {
                *self = saved;
                Err(error)
            }
        }
    }

    fn update_vblanks_with_backend<C: CameraCollisionBackend>(
        &mut self,
        projection: WorldProjection,
        collision: &mut C,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
        delta_vblanks: u16,
    ) -> Result<ThirdPersonCameraFrame, CollisionQueryError> {
        self.profile.set_base_projection(projection);
        let steps = delta_vblanks.clamp(1, MAX_CAMERA_CATCHUP_VBLANKS);
        let config = normalize_config(config);
        let mut i = 0;
        while i < steps {
            self.advance_one_vblank(
                collision,
                target,
                ThirdPersonCameraInput {
                    recenter: input.recenter && i == 0,
                    ..input
                },
                config,
            )?;
            i += 1;
        }
        Ok(self.current_frame(projection))
    }

    fn advance_one_vblank<C: CameraCollisionBackend>(
        &mut self,
        collision: &mut C,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
    ) -> Result<(), CollisionQueryError> {
        if !self.initialized {
            self.snap_to_player(target, config);
        }

        let previous_default_pitch = default_pitch_q12(self.profile.apply(config));
        self.profile.advance(config, target.lock_target.is_some());
        let mut config = self.profile.apply(config);
        if config.lock_target_framing && config.fov_y_degrees == 0 {
            config.fov_y_degrees = self.profile.vertical_fov_degrees();
        }
        // Preserve the player's orbit adjustment as the authored composition changes.
        if !config.preserve_profile_pitch && (config.blend_profiles || config.lock_profile.is_some()) {
            self.pitch_q12 = self
                .pitch_q12
                .saturating_add(default_pitch_q12(config).saturating_sub(previous_default_pitch))
                .clamp(config.pitch_min_q12, config.pitch_max_q12);
        }
        if config.lock_target_framing && target.lock_target.is_some() {
            config.focus_lag_shift = config.focus_lag_shift.min(2);
            config.focus_vertical_lag_shift = Some(
                config
                    .focus_vertical_lag_shift
                    .unwrap_or(config.focus_lag_shift)
                    .min(3),
            );
        }
        let mut focus_goal = camera_focus_goal(target, config, self.distance);
        // The orbit the previous frame settled on, before input and steering.
        let previous_yaw = self.yaw;

        if input.recenter {
            self.recenter_active = true;
            self.recenter_remaining = 18;
        }
        if target.lock_target.is_some() {
            self.recenter_active = false;
        }

        let yaw_delta = accelerated_orbit_delta(
            input.yaw_delta_q12,
            config.accelerated_orbit_speed,
            &mut self.yaw_ramp,
            &mut self.yaw_remainder,
        );
        let pitch_delta = accelerated_orbit_delta(
            input.pitch_delta_q12,
            config.accelerated_orbit_speed,
            &mut self.pitch_ramp,
            &mut self.pitch_remainder,
        );
        if input.yaw_delta_q12 != 0 || input.pitch_delta_q12 != 0 {
            self.recenter_active = false;
            self.yaw = self.yaw.add_signed_q12(yaw_delta);
            self.pitch_q12 = self
                .pitch_q12
                .saturating_add(pitch_delta)
                .clamp(config.pitch_min_q12, config.pitch_max_q12);
            self.manual_cooldown = config.manual_cooldown_frames;
            self.manual_release = config.manual_release_frames;
        } else if self.manual_cooldown != 0 {
            self.manual_cooldown -= 1;
        } else if self.manual_release != 0 {
            self.manual_release -= 1;
        }

        // Shift the pivot toward the shoulder before either collision sweep.
        // Use the player-to-enemy axis so the offset cannot feed back into yaw.
        let shoulder_yaw = target.lock_target.map_or(self.yaw, |lock| {
            yaw_to_point(target.player, lock).add(Angle::HALF)
        });
        focus_goal.x = focus_goal.x.saturating_add(shoulder_yaw.cos().mul_i32(config.shoulder_offset));
        focus_goal.z = focus_goal.z.saturating_sub(shoulder_yaw.sin().mul_i32(config.shoulder_offset));
        let player_back_yaw = target.player_yaw.add(Angle::HALF);
        let (desired_yaw, yaw_step) = if let Some(lock) = target.lock_target {
            let dx = lock.x.saturating_sub(target.player.x).saturating_abs();
            let dz = lock.z.saturating_sub(target.player.z).saturating_abs();
            let close_radius = (config.distance / 6).max(8);
            // At body contact the target bearing becomes unstable and flips
            // by 180 degrees when it crosses the player. Keep the orbit steady
            // in that small zone; focus still follows both actors.
            let bearing = if dx.max(dz) < close_radius {
                self.yaw
            } else {
                // Aim from the shifted pivot, keeping the enemy central while
                // the player remains to the left of the firing sightline.
                yaw_to_point(if config.shoulder_offset == 0 { target.player } else { focus_goal }, lock)
                    .add(Angle::HALF)
            };
            (bearing, config.lock_on_align_step)
        } else if self.recenter_active
            || (config.auto_align_when_moving && target.moving && self.manual_cooldown == 0)
        {
            (
                player_back_yaw,
                if self.recenter_active {
                    if config.recenter_preserves_pitch {
                        let remaining = u16::from(self.recenter_remaining.max(1));
                        let error = self.yaw.shortest_delta_q12(player_back_yaw).unsigned_abs();
                        Angle::from_q12(error.div_ceil(remaining))
                    } else {
                        config.lock_on_align_step
                    }
                } else if config.manual_release_frames != 0 {
                    let released = config
                        .manual_release_frames
                        .saturating_sub(self.manual_release);
                    Angle::from_q12(
                        (u32::from(config.auto_align_step.as_q12()) * u32::from(released)
                            / u32::from(config.manual_release_frames))
                            as u16,
                    )
                } else {
                    config.auto_align_step
                },
            )
        } else {
            (self.yaw, config.auto_align_step)
        };
        self.yaw = self.yaw.approach_q12(desired_yaw, yaw_step.as_q12());
        if self.recenter_active {
            self.recenter_remaining = self.recenter_remaining.saturating_sub(1);
            let recenter_pitch = if config.recenter_preserves_pitch {
                self.pitch_q12
            } else {
                default_pitch_q12(config)
            };
            self.pitch_q12 = approach_i16(
                self.pitch_q12,
                recenter_pitch,
                config.lock_on_align_step.as_q12() as i16,
            );
            if self.yaw == player_back_yaw && self.pitch_q12 == recenter_pitch {
                self.recenter_active = false;
            }
        }

        let previous_focus = self.focus;
        let mut proposed_focus = approach_vertex_shift(
            self.focus,
            focus_goal,
            config.focus_lag_shift,
            config
                .focus_vertical_lag_shift
                .unwrap_or(config.focus_lag_shift),
        );
        if config
            .focus_vertical_lag_shift
            .is_some_and(|vertical| vertical > config.focus_lag_shift)
        {
            // A long vertical tail works on steps, but after a drop it can
            // leave the focus above the player while collision pulls the eye
            // below it. Bound that tail before checking the focus segment;
            // camera clearance must still be able to override the framing.
            let vertical_slack = (config.target_height / 4).max(1);
            proposed_focus.y = proposed_focus.y.clamp(
                focus_goal.y.saturating_sub(vertical_slack),
                focus_goal.y.saturating_add(vertical_slack),
            );
        }
        // Lock bias and follow lag can put the look-at point through a pillar.
        // Start below the look-at height, which may sit above the player's
        // collision body and enter a low ceiling.
        let anchor_height = (config.target_height / 2)
            .max(config.min_floor_clearance)
            .min(config.target_height);
        self.focus = collision.constrain_segment(
            player_focus(target.player, anchor_height),
            proposed_focus,
            config,
        )?;
        // Translation follows the focus; lag smooths changes to the orbit.
        // Otherwise a slow camera can remain behind a moving player while
        // its look-at point moves ahead and puts the body outside the frame.
        self.position.x = self
            .position
            .x
            .saturating_add(self.focus.x.saturating_sub(previous_focus.x));
        self.position.z = self
            .position
            .z
            .saturating_add(self.focus.z.saturating_sub(previous_focus.z));
        self.base_position_y = self
            .base_position_y
            .saturating_add(self.focus.y.saturating_sub(previous_focus.y));

        // Reference framing keeps the focus above the player and moves the
        // orbit around it. Keep manual pitch separately so unlock can restore it.
        let lock_pitch_goal = if config.lock_target_framing {
            target
                .lock_target
                .and_then(|lock| lock_pitch_goal(self.focus, lock, self.distance, config))
        } else {
            None
        };
        let offset_goal = lock_pitch_goal.map_or(0, |pitch| pitch - self.pitch_q12);
        let old_pitch_offset = self.lock_pitch_offset_q12;
        // 1-sqrt(1-.3) in Q12, adapting the native 30 Hz chase to 60 Hz.
        let error = i32::from(offset_goal) - i32::from(self.lock_pitch_offset_q12);
        let step = error * 669 / 4096;
        self.lock_pitch_offset_q12 += if step == 0 { error.signum() } else { step } as i16;
        let orbit_pitch = self
            .pitch_q12
            .saturating_add(self.lock_pitch_offset_q12)
            .clamp(config.pitch_min_q12, config.pitch_max_q12);

        // Legacy framing retains its authored height lift. Angular framing
        // already accounts for elevation, so adding that lift would count twice.
        let lock_height_goal = if target.lock_target.is_some() && !config.lock_target_framing {
            config.lock_height_boost
        } else {
            0
        };
        self.lock_height_offset = approach_i32_shift(
            self.lock_height_offset,
            lock_height_goal,
            config.focus_lag_shift.saturating_add(2),
        )
        .clamp(0, config.lock_height_boost);
        let base_camera_y_goal = camera_height_goal(target.player, orbit_pitch, config);
        let locked_camera_y_goal = base_camera_y_goal.saturating_add(self.lock_height_offset);

        // Spring-arm sweep throttle: the sweep dominates the camera's
        // per-tick cost, and between solves the focus/yaw move by one
        // tick of easing, so reusing the previous solve only delays
        // collision reaction by up to (interval - 1) ticks. Deliberate
        // camera moves (manual orbit, lock-on, recenter) always solve
        // fresh so the throttle never fights the player's hand.
        let solve_now = self.solve_phase == 0
            || self.profile.changing()
            || old_pitch_offset != self.lock_pitch_offset_q12
            || input.yaw_delta_q12 != 0
            || input.pitch_delta_q12 != 0
            || input.recenter
            || self.recenter_active
            || target.lock_target.is_some();
        self.solve_phase = self.solve_phase.saturating_add(1);
        if self.solve_phase >= config.collision_solve_interval.max(1) {
            self.solve_phase = 0;
        }
        let mut swung = false;
        let collision_solve = if solve_now {
            let mut solve = collision.solve(
                self.focus,
                self.yaw,
                orbit_pitch,
                locked_camera_y_goal,
                config,
            )?;
            let wanted = clear_orbit_distance(config);
            if input.yaw_delta_q12 != 0 || target.lock_target.is_none() {
                self.clear_orbit_hold = false;
            } else if self.clear_orbit_hold {
                // A clear incremental steering step does not mean the target
                // bearing is clear. Hold the side view until that full bearing
                // reopens, otherwise steering repeatedly collapses the arm.
                let desired = if self.yaw == desired_yaw {
                    solve
                } else {
                    collision.solve(
                        self.focus,
                        desired_yaw,
                        orbit_pitch,
                        locked_camera_y_goal,
                        config,
                    )?
                };
                if desired.distance >= wanted {
                    self.clear_orbit_hold = false;
                } else if self.yaw != previous_yaw {
                    self.yaw = previous_yaw;
                    solve = collision.solve(
                        self.focus,
                        previous_yaw,
                        orbit_pitch,
                        locked_camera_y_goal,
                        config,
                    )?;
                }
            }
            if target.lock_target.is_some()
                && solve.distance < clear_orbit_trigger(config, self.clear_orbit_hold)
            {
                // Locked on, with the arm blocked at the player's own body
                // (back to a wall or pillar): the eye would sit in or against
                // her head. Lock-on already owns the orbit yaw, so swing it to
                // a clear side. A free camera keeps its yaw, and with it the
                // stick directions the player steers by (swinging it even for
                // a player standing still re-routed the whole-level tape at
                // its first pause); the scene hides the player model instead
                // while the arm is that short.
                if let Some((yaw, clear)) = clear_orbit_yaw(
                    collision,
                    self.focus,
                    self.yaw,
                    previous_yaw.shortest_delta_q12(self.yaw),
                    solve.distance,
                    orbit_pitch,
                    locked_camera_y_goal,
                    config,
                )? {
                    swung = true;
                    self.yaw = yaw;
                    self.recenter_active = false;
                    self.clear_orbit_hold = true;
                    solve = clear;
                }
            }
            self.cached_solve = solve;
            solve
        } else {
            self.cached_solve
        };

        if swung {
            // A cut to the clear side: easing the arm or the eye from the
            // collapsed spot would drag the camera through the wall.
            self.distance = collision_solve.distance;
            self.collision_release_delay = config.collision_release_delay_frames;
        } else if collision_solve.distance < self.distance {
            self.distance = collision_solve.distance;
            self.collision_release_delay = config.collision_release_delay_frames;
        } else if self.collision_release_delay != 0 {
            self.collision_release_delay -= 1;
        } else {
            self.distance = approach_i32_shift(
                self.distance,
                collision_solve.distance,
                config.distance_lag_shift,
            );
        }

        // Spring arm: a shortened boom slides the camera along the arm's own
        // direction toward the focus, so the height comes down with the
        // distance and the pitch holds. Holding the full height while the arm
        // collapses parks the camera almost directly above the player,
        // looking straight down. Scale the lock-on lift along that same ray.
        let base_camera_y_goal = if self.distance < config.distance {
            let above_focus = base_camera_y_goal.saturating_sub(self.focus.y);
            self.focus
                .y
                .saturating_add(above_focus.saturating_mul(self.distance) / config.distance.max(1))
        } else {
            base_camera_y_goal
        };
        let desired_base_position = camera_position_at_height(
            self.focus,
            self.distance,
            self.yaw,
            orbit_pitch,
            base_camera_y_goal,
        );
        if collision_solve.pull_in || swung {
            self.position.x = desired_base_position.x;
            self.position.z = desired_base_position.z;
            self.base_position_y = base_camera_y_goal;
        } else {
            self.position.x = approach_i32_shift(
                self.position.x,
                desired_base_position.x,
                config.position_lag_shift,
            );
            self.position.z = approach_i32_shift(
                self.position.z,
                desired_base_position.z,
                config.position_lag_shift,
            );
            self.base_position_y = approach_i32_shift(
                self.base_position_y,
                base_camera_y_goal,
                config
                    .position_vertical_lag_shift
                    .unwrap_or(config.position_lag_shift),
            );
        }
        let lock_lift =
            self.lock_height_offset.saturating_mul(self.distance) / config.distance.max(1);
        self.position.y = self.base_position_y.saturating_add(lock_lift);
        let before_floor = self.position;
        self.position = collision.clamp_to_floor(self.position, config.min_floor_clearance)?;
        let mut endpoint_clamped = false;
        if !solve_now || !collision_solve.pull_in || self.position != before_floor {
            // Easing between two clear orbit endpoints can cut through their
            // shared corner. A cached shortened boom also needs validation
            // after the player moves. Validate the rendered position.
            let safe = collision.constrain_segment(self.focus, self.position, config)?;
            if safe != self.position {
                self.position = safe;
                self.base_position_y = safe.y.saturating_sub(lock_lift);
                let dx = safe.x.saturating_sub(self.focus.x);
                let dz = safe.z.saturating_sub(self.focus.z);
                let horizontal =
                    isqrt_i32(dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz)));
                // `distance` is the boom length: the orbit applies cos(pitch)
                // to obtain its horizontal radius. Storing the radius here
                // would apply that shortening a second time on the next tick.
                let cos_pitch = signed_q12_angle(orbit_pitch).cos().raw().saturating_abs();
                self.distance = div_q12_i32(horizontal, cos_pitch.max(1)).min(self.distance);
                self.collision_release_delay = config.collision_release_delay_frames;
                self.solve_phase = 0;
                endpoint_clamped = true;
            }
        }
        self.frame_pitch_q12 =
            orbit_pitch.saturating_add(lock_pitch_offset_q12(config, self.lock_height_offset));

        self.last_pull_in = collision_solve.pull_in || endpoint_clamped;
        self.last_rotated = false;
        Ok(())
    }

    fn current_frame(&self, projection: WorldProjection) -> ThirdPersonCameraFrame {
        ThirdPersonCameraFrame {
            camera: camera_from_position_focus(
                self.projection(projection),
                self.position,
                self.focus,
                self.yaw,
            ),
            focus: self.focus,
            yaw: self.yaw,
            pitch_q12: self.frame_pitch_q12,
            distance: self.distance,
            collision_pull_in: self.last_pull_in,
            collision_rotated: self.last_rotated,
        }
    }

    /// End target tracking without restoring the pre-lock viewing pitch.
    pub fn release_lock_preserving_view(&mut self) {
        // Absorb tracking pitch into manual orbit rather than springing back to
        // the pre-lock angle when a gun remains raised after target loss.
        self.pitch_q12 = self.frame_pitch_q12;
        self.lock_pitch_offset_q12 = 0;
        self.recenter_active = false;
        self.clear_orbit_hold = false;
        self.yaw_ramp = 0;
        self.pitch_ramp = 0;
    }

    /// Projection shared by geometry, sky, visibility and overlays for this camera.
    pub fn projection(&self, base: WorldProjection) -> WorldProjection {
        self.profile.projection(base)
    }

    /// Current orbit yaw.
    pub const fn yaw(&self) -> Angle {
        self.yaw
    }

    /// Current orbit pitch in signed Q0.12 units.
    pub const fn pitch_q12(&self) -> i16 {
        self.pitch_q12
    }

    /// Current camera position.
    pub const fn position(&self) -> RoomPoint {
        self.position
    }

    /// Current lagged focus point.
    pub const fn focus(&self) -> RoomPoint {
        self.focus
    }

    /// Current arm length after collision. Under the config's
    /// `min_distance` the eye is inside the player's body.
    pub const fn distance(&self) -> i32 {
        self.distance
    }
}

/// Orbit step tried when the arm collapses inside the player (1/16 turn).
const CLEAR_ORBIT_STEP_Q12: i16 = 256;
/// Steps tried each way, so the search reaches all the way round.
const CLEAR_ORBIT_STEPS: i16 = 8;

/// Arm length below which the camera looks for a clearer orbit. From a
/// normal orbit only a collapse under `min_distance` (the eye inside the
/// player) swings it, so ordinary close walls and corridors keep the plain
/// spring arm and the stick directions it gives. Once swung, the camera keeps
/// looking while its arm is under twice that, so it settles on a view of the
/// whole body rather than the first spot that clears her head.
fn clear_orbit_trigger(config: ThirdPersonCameraConfig, swung: bool) -> i32 {
    if swung {
        config.min_distance.saturating_mul(2)
    } else {
        config.min_distance
    }
}

/// Arm length an orbit swung off a wall should reach: half the preferred
/// distance, and at least twice `min_distance`.
fn clear_orbit_distance(config: ThirdPersonCameraConfig) -> i32 {
    (config.distance / 2)
        .max(config.min_distance.saturating_mul(2))
        .min(config.distance)
}

/// A nearby orbit yaw whose arm clears the player's body, for an arm that
/// collapsed below [`clear_orbit_trigger`]. The search fans out from `yaw`, turning
/// first against `steering_q12` (the way steering just moved it), and takes
/// the first arm of [`clear_orbit_distance`], else the longest one past
/// `min_distance`. A candidate must improve clearance by at least the collision
/// margin. `None` keeps the current arm when no better view is available.
fn clear_orbit_yaw<C: CameraCollisionBackend>(
    collision: &mut C,
    focus: RoomPoint,
    yaw: Angle,
    steering_q12: i16,
    current_distance: i32,
    pitch_q12: i16,
    camera_y: i32,
    config: ThirdPersonCameraConfig,
) -> Result<Option<(Angle, CollisionSolve)>, CollisionQueryError> {
    let wanted = clear_orbit_distance(config);
    let minimum = current_distance
        .saturating_add(config.collision_margin.max(1))
        .max(config.min_distance);
    let away: i16 = if steering_q12 > 0 { -1 } else { 1 };
    let mut best: Option<(Angle, CollisionSolve)> = None;
    let mut step = 1;
    while step <= CLEAR_ORBIT_STEPS {
        for side in [away, -away] {
            if step == CLEAR_ORBIT_STEPS && side != away {
                // Both ways meet at the half turn; trace it once.
                continue;
            }
            let candidate = yaw.add_signed_q12(side * step * CLEAR_ORBIT_STEP_Q12);
            let solve = collision.solve(focus, candidate, pitch_q12, camera_y, config)?;
            if solve.distance < minimum {
                continue;
            }
            if solve.distance >= wanted {
                return Ok(Some((candidate, solve)));
            }
            if solve.distance >= config.min_distance
                && best.is_none_or(|(_, longest)| solve.distance > longest.distance)
            {
                best = Some((candidate, solve));
            }
        }
        step += 1;
    }
    Ok(best)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct CollisionSolve {
    distance: i32,
    pull_in: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct CameraRay {
    from: RoomPoint,
    to: RoomPoint,
    dx: i32,
    dy: i32,
    dz: i32,
    distance: i32,
    sector_size: i32,
    room_width: i32,
    room_depth: i32,
    vertical_margin: i32,
}

#[derive(Copy, Clone, Debug)]
enum CameraCollision<'room, 'room_ref, 'rooms> {
    Single(Option<RoomCollision<'room, 'room_ref>>),
    Rooms(&'rooms [CharacterCollisionRoom<'room>]),
}

trait CameraCollisionBackend {
    fn constrain_segment(
        &mut self,
        start: RoomPoint,
        end: RoomPoint,
        config: ThirdPersonCameraConfig,
    ) -> Result<RoomPoint, CollisionQueryError>;

    fn solve(
        &mut self,
        focus: RoomPoint,
        yaw: Angle,
        pitch_q12: i16,
        camera_y: i32,
        config: ThirdPersonCameraConfig,
    ) -> Result<CollisionSolve, CollisionQueryError>;

    fn clamp_to_floor(
        &mut self,
        position: RoomPoint,
        min_floor_clearance: i32,
    ) -> Result<RoomPoint, CollisionQueryError>;
}

struct GridCameraCollision<'room, 'room_ref, 'rooms> {
    collision: CameraCollision<'room, 'room_ref, 'rooms>,
}

impl CameraCollisionBackend for GridCameraCollision<'_, '_, '_> {
    fn constrain_segment(
        &mut self,
        start: RoomPoint,
        end: RoomPoint,
        mut config: ThirdPersonCameraConfig,
    ) -> Result<RoomPoint, CollisionQueryError> {
        let span = segment_span(start, end);
        if span == 0 {
            return Ok(end);
        }
        config.distance = span;
        let clear = match self.collision {
            CameraCollision::Single(Some(room)) => {
                probe_clear_distance(room, start, end, span, config)
            }
            CameraCollision::Single(None) => span,
            CameraCollision::Rooms(rooms) => {
                probe_clear_distance_rooms(rooms, start, end, span, config)
            }
        };
        Ok(lerp_clear_segment(start, end, clear, span))
    }

    fn solve(
        &mut self,
        focus: RoomPoint,
        yaw: Angle,
        pitch_q12: i16,
        camera_y: i32,
        config: ThirdPersonCameraConfig,
    ) -> Result<CollisionSolve, CollisionQueryError> {
        Ok(solve_camera_collision_context(
            self.collision,
            focus,
            yaw,
            pitch_q12,
            camera_y,
            config,
        ))
    }

    fn clamp_to_floor(
        &mut self,
        position: RoomPoint,
        min_floor_clearance: i32,
    ) -> Result<RoomPoint, CollisionQueryError> {
        Ok(clamp_camera_to_floor_context(
            self.collision,
            position,
            min_floor_clearance,
        ))
    }
}

struct TraceCameraCollision<'provider, P: ?Sized> {
    provider: &'provider mut P,
}

impl<P: CollisionTraceProvider + ?Sized> CameraCollisionBackend for TraceCameraCollision<'_, P> {
    fn constrain_segment(
        &mut self,
        start: RoomPoint,
        end: RoomPoint,
        config: ThirdPersonCameraConfig,
    ) -> Result<RoomPoint, CollisionQueryError> {
        let span = segment_span(start, end);
        if span == 0 {
            return Ok(end);
        }
        let trace = trace_collision(self.provider, CollisionTraceQuery::point(start, end))?;
        if trace.start_solid || trace.all_solid {
            return Ok(start);
        }
        if !trace.hit() {
            return Ok(end);
        }
        let clear = mul_q12_i32(span, trace.fraction_q12)
            .saturating_sub(config.collision_margin)
            .max(0);
        Ok(lerp_clear_segment(start, end, clear, span))
    }

    fn solve(
        &mut self,
        focus: RoomPoint,
        yaw: Angle,
        pitch_q12: i16,
        camera_y: i32,
        config: ThirdPersonCameraConfig,
    ) -> Result<CollisionSolve, CollisionQueryError> {
        solve_camera_collision_trace(self.provider, focus, yaw, pitch_q12, camera_y, config)
    }

    fn clamp_to_floor(
        &mut self,
        position: RoomPoint,
        min_floor_clearance: i32,
    ) -> Result<RoomPoint, CollisionQueryError> {
        clamp_camera_to_floor_trace(self.provider, position, min_floor_clearance)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct CheckedCameraCells {
    bitset: [u32; CHECKED_CAMERA_CELL_WORDS],
    cells: [u32; MAX_RAY_CHECKED_CELLS],
    len: usize,
}

impl CheckedCameraCells {
    const EMPTY_CELL: u32 = u32::MAX;

    const fn new() -> Self {
        Self {
            bitset: [0; CHECKED_CAMERA_CELL_WORDS],
            cells: [Self::EMPTY_CELL; MAX_RAY_CHECKED_CELLS],
            len: 0,
        }
    }

    fn visit(&mut self, key: u32) -> bool {
        let word = (key / 32) as usize;
        if word < self.bitset.len() {
            let mask = 1u32 << (key & 31);
            if self.bitset[word] & mask != 0 {
                return false;
            }
            self.bitset[word] |= mask;
            return true;
        }

        let mut i = 0;
        while i < self.len {
            if self.cells[i] == key {
                return false;
            }
            i += 1;
        }
        if self.len < self.cells.len() {
            self.cells[self.len] = key;
            self.len += 1;
        }
        true
    }
}

fn normalize_config(mut config: ThirdPersonCameraConfig) -> ThirdPersonCameraConfig {
    config.min_distance = config.min_distance.max(8);
    config.max_distance = config.max_distance.max(config.min_distance);
    config.distance = config
        .distance
        .clamp(config.min_distance, config.max_distance);
    config.collision_margin = config.collision_margin.max(0);
    config.min_floor_clearance = config.min_floor_clearance.max(0);
    if config.pitch_min_q12 > config.pitch_max_q12 {
        core::mem::swap(&mut config.pitch_min_q12, &mut config.pitch_max_q12);
    }
    if config.auto_align_step == Angle::ZERO {
        config.auto_align_step = Angle::from_q12(1);
    }
    if config.lock_on_align_step == Angle::ZERO {
        config.lock_on_align_step = config.auto_align_step;
    }
    config.position_lag_shift = config.position_lag_shift.min(6);
    config.position_vertical_lag_shift =
        config.position_vertical_lag_shift.map(|shift| shift.min(6));
    config.focus_lag_shift = config.focus_lag_shift.min(6);
    config.focus_vertical_lag_shift = config.focus_vertical_lag_shift.map(|shift| shift.min(6));
    config.distance_lag_shift = config.distance_lag_shift.min(6);
    config.lock_height_boost = config.lock_height_boost.max(0);
    config.collision_solve_interval = config.collision_solve_interval.clamp(1, 4);
    config
}

fn player_focus(player: RoomPoint, target_height: i32) -> RoomPoint {
    RoomPoint::new(player.x, player.y.saturating_add(target_height), player.z)
}

fn lerp_clear_segment(start: RoomPoint, end: RoomPoint, clear: i32, span: i32) -> RoomPoint {
    if clear >= span {
        return end;
    }
    let fraction = div_q12_i32(clear.max(0), span);
    RoomPoint::new(
        start
            .x
            .saturating_add(mul_q12_i32(end.x.saturating_sub(start.x), fraction)),
        start
            .y
            .saturating_add(mul_q12_i32(end.y.saturating_sub(start.y), fraction)),
        start
            .z
            .saturating_add(mul_q12_i32(end.z.saturating_sub(start.z), fraction)),
    )
}

fn segment_span(start: RoomPoint, end: RoomPoint) -> i32 {
    end.x
        .saturating_sub(start.x)
        .saturating_abs()
        .max(end.y.saturating_sub(start.y).saturating_abs())
        .max(end.z.saturating_sub(start.z).saturating_abs())
}

fn camera_focus_goal(
    target: ThirdPersonCameraTarget,
    config: ThirdPersonCameraConfig,
    arm_distance: i32,
) -> RoomPoint {
    let player = player_focus(target.player, config.target_height);
    let Some(lock) = target.lock_target else {
        return player;
    };

    if config.lock_target_framing {
        return player;
    }
    // Legacy composition biases a quarter of the way toward the target.
    let blended = lerp_vertex(player, player_focus(lock, config.target_height), 1, 4);
    let max_offset = (arm_distance.min(config.distance) / 3).max(0);
    RoomPoint::new(
        player.x.saturating_add(
            blended
                .x
                .saturating_sub(player.x)
                .clamp(-max_offset, max_offset),
        ),
        player.y,
        player.z.saturating_add(
            blended
                .z
                .saturating_sub(player.z)
                .clamp(-max_offset, max_offset),
        ),
    )
}

fn clamp_camera_to_floor_trace<P: CollisionTraceProvider + ?Sized>(
    provider: &mut P,
    position: RoomPoint,
    min_floor_clearance: i32,
) -> Result<RoomPoint, CollisionQueryError> {
    if min_floor_clearance <= 0 {
        return Ok(position);
    }
    let start = position.with_y(position.y.saturating_add(TRACE_CAMERA_FLOOR_PROBE_LIFT));
    let end = position.with_y(position.y.saturating_sub(TRACE_CAMERA_FLOOR_PROBE_DOWN));
    let trace = trace_collision(provider, CollisionTraceQuery::point(start, end))?;
    if trace.start_solid
        || trace.all_solid
        || trace.fraction_q12 >= COLLISION_FRACTION_ONE_Q12
        || trace.normal_q12[1] <= 0
    {
        return Ok(position);
    }
    let Some(min_y) = trace.end.y.checked_add(min_floor_clearance) else {
        return Ok(position);
    };
    if position.y < min_y {
        Ok(position.with_y(min_y))
    } else {
        Ok(position)
    }
}

fn solve_camera_collision_trace<P: CollisionTraceProvider + ?Sized>(
    provider: &mut P,
    focus: RoomPoint,
    yaw: Angle,
    pitch_q12: i16,
    camera_y: i32,
    config: ThirdPersonCameraConfig,
) -> Result<CollisionSolve, CollisionQueryError> {
    let desired = camera_position_at_height(focus, config.distance, yaw, pitch_q12, camera_y);
    let trace = trace_collision(provider, CollisionTraceQuery::point(focus, desired))?;
    if !trace.hit() && !trace.all_solid {
        return Ok(CollisionSolve {
            distance: config.distance,
            pull_in: false,
        });
    }
    let fraction = trace.fraction_q12.clamp(0, COLLISION_FRACTION_ONE_Q12);
    let clear = if trace.start_solid || trace.all_solid {
        0
    } else {
        mul_q12_i32(config.distance.max(1), fraction)
    };
    let distance = clear
        .saturating_sub(config.collision_margin)
        .clamp(0, config.distance);
    Ok(CollisionSolve {
        distance,
        pull_in: distance < config.distance,
    })
}

fn clamp_camera_to_floor(
    collision: Option<RoomCollision<'_, '_>>,
    position: RoomPoint,
    min_floor_clearance: i32,
) -> RoomPoint {
    let Some(room) = collision else {
        return position;
    };
    if min_floor_clearance <= 0 {
        return position;
    }
    let Some(floor_y) = floor_height_at(room, position.x, position.z) else {
        return position;
    };
    let Some(min_y) = floor_y.checked_add(min_floor_clearance) else {
        return position;
    };
    if position.y < min_y {
        RoomPoint::new(position.x, min_y, position.z)
    } else {
        position
    }
}

fn clamp_camera_to_floor_context(
    collision: CameraCollision<'_, '_, '_>,
    position: RoomPoint,
    min_floor_clearance: i32,
) -> RoomPoint {
    match collision {
        CameraCollision::Single(room) => clamp_camera_to_floor(room, position, min_floor_clearance),
        CameraCollision::Rooms(rooms) => {
            clamp_camera_to_floor_rooms(rooms, position, min_floor_clearance)
        }
    }
}

fn clamp_camera_to_floor_rooms(
    rooms: &[CharacterCollisionRoom<'_>],
    position: RoomPoint,
    min_floor_clearance: i32,
) -> RoomPoint {
    if min_floor_clearance <= 0 {
        return position;
    }
    let Some(floor_y) = floor_height_at_rooms(rooms, position) else {
        return position;
    };
    let Some(min_y) = floor_y.checked_add(min_floor_clearance) else {
        return position;
    };
    if position.y < min_y {
        RoomPoint::new(position.x, min_y, position.z)
    } else {
        position
    }
}

fn floor_height_at_rooms(rooms: &[CharacterCollisionRoom<'_>], point: RoomPoint) -> Option<i32> {
    let mut i = 0usize;
    while i < rooms.len() && i < MAX_CAMERA_COLLISION_ROOMS {
        let entry = rooms[i];
        if let Some(room) = entry.room {
            let local = collision_room_local_point(entry, point);
            if let Some(height) = floor_height_at(room.collision(), local.x, local.z) {
                return Some(height);
            }
        }
        i += 1;
    }
    None
}

fn collision_room_local_point(entry: CharacterCollisionRoom<'_>, point: RoomPoint) -> RoomPoint {
    RoomPoint::new(
        point.x.saturating_sub(entry.offset_x),
        point.y,
        point.z.saturating_sub(entry.offset_z),
    )
}

fn floor_height_at(room: RoomCollision<'_, '_>, x: i32, z: i32) -> Option<i32> {
    let s = room.sector_size();
    if s <= 0 || x < 0 || z < 0 {
        return None;
    }
    let sx = x / s;
    let sz = z / s;
    if sx < 0 || sz < 0 || sx >= room.width() as i32 || sz >= room.depth() as i32 {
        return None;
    }
    let local_x = (x - sx * s).clamp(0, s);
    let local_z = (z - sz * s).clamp(0, s);
    let sector = room.sector_floor_collision(sx as u16, sz as u16, local_x, local_z, s)?;
    let heights = triangle_heights_to_quad(
        sector.floor_heights(),
        sector.split(),
        sector.triangle(),
        sector.triangle_heights(),
    );
    Some(height_at_local(
        heights,
        sector.split(),
        local_x,
        local_z,
        s,
    ))
}

fn solve_camera_collision(
    collision: Option<RoomCollision<'_, '_>>,
    focus: RoomPoint,
    yaw: Angle,
    pitch_q12: i16,
    camera_y: i32,
    config: ThirdPersonCameraConfig,
) -> CollisionSolve {
    let Some(room) = collision else {
        return CollisionSolve {
            distance: config.distance,
            pull_in: false,
        };
    };

    let desired = camera_position_at_height(focus, config.distance, yaw, pitch_q12, camera_y);
    let clear = probe_clear_distance(room, focus, desired, config.distance, config);
    let distance = clear.clamp(0, config.distance);
    CollisionSolve {
        distance,
        pull_in: distance < config.distance,
    }
}

fn solve_camera_collision_context(
    collision: CameraCollision<'_, '_, '_>,
    focus: RoomPoint,
    yaw: Angle,
    pitch_q12: i16,
    camera_y: i32,
    config: ThirdPersonCameraConfig,
) -> CollisionSolve {
    match collision {
        CameraCollision::Single(room) => {
            solve_camera_collision(room, focus, yaw, pitch_q12, camera_y, config)
        }
        CameraCollision::Rooms(rooms) => {
            solve_camera_collision_rooms(rooms, focus, yaw, pitch_q12, camera_y, config)
        }
    }
}

fn solve_camera_collision_rooms(
    rooms: &[CharacterCollisionRoom<'_>],
    focus: RoomPoint,
    yaw: Angle,
    pitch_q12: i16,
    camera_y: i32,
    config: ThirdPersonCameraConfig,
) -> CollisionSolve {
    if rooms.is_empty() {
        return CollisionSolve {
            distance: config.distance,
            pull_in: false,
        };
    }

    let desired = camera_position_at_height(focus, config.distance, yaw, pitch_q12, camera_y);
    let clear = probe_clear_distance_rooms(rooms, focus, desired, config.distance, config);
    let distance = clear.clamp(0, config.distance);
    CollisionSolve {
        distance,
        pull_in: distance < config.distance,
    }
}

fn probe_clear_distance(
    room: RoomCollision<'_, '_>,
    from: RoomPoint,
    to: RoomPoint,
    max_distance: i32,
    config: ThirdPersonCameraConfig,
) -> i32 {
    let max_distance = max_distance.max(1);
    let sector = room.sector_size().max(1);
    let ray = CameraRay {
        from,
        to,
        dx: to.x.saturating_sub(from.x),
        dy: to.y.saturating_sub(from.y),
        dz: to.z.saturating_sub(from.z),
        distance: max_distance,
        sector_size: sector,
        room_width: room.width() as i32,
        room_depth: room.depth() as i32,
        vertical_margin: config.collision_margin,
    };
    let mut steps = (max_distance / (sector / 4).max(1)).clamp(RAY_STEPS_MIN, RAY_STEPS_MAX);
    if steps <= 0 {
        steps = RAY_STEPS_MIN;
    }

    let mut nearest = max_distance;
    let mut last_clear_distance = 0;
    let mut checked_cells = CheckedCameraCells::new();
    let mut i = 1;
    while i <= steps {
        let sample = lerp_vertex(from, to, i, steps);
        if point_outside_camera_space(room, sample, sector, ray.room_width, ray.room_depth) {
            nearest = last_clear_distance.min(nearest);
            break;
        }
        if let Some(hit) = nearest_wall_hit_around(room, sample, ray, &mut checked_cells) {
            nearest = hit.min(nearest);
            break;
        }
        last_clear_distance = (max_distance.saturating_mul(i)) / steps;
        i += 1;
    }

    if last_clear_distance == max_distance {
        max_distance
    } else {
        nearest
            .saturating_sub(config.collision_margin)
            .clamp(0, config.distance)
    }
}

fn probe_clear_distance_rooms(
    rooms: &[CharacterCollisionRoom<'_>],
    from: RoomPoint,
    to: RoomPoint,
    max_distance: i32,
    config: ThirdPersonCameraConfig,
) -> i32 {
    let Some(sector) = first_collision_room_sector_size(rooms) else {
        return config.distance;
    };
    let max_distance = max_distance.max(1);
    let mut steps = (max_distance / (sector / 4).max(1)).clamp(RAY_STEPS_MIN, RAY_STEPS_MAX);
    if steps <= 0 {
        steps = RAY_STEPS_MIN;
    }

    let mut nearest = max_distance;
    let mut last_clear_distance = 0;
    let mut checked_cells = [const { CheckedCameraCells::new() }; MAX_CAMERA_COLLISION_ROOMS];
    let mut i = 1;
    while i <= steps {
        let sample = lerp_vertex(from, to, i, steps);
        if point_outside_camera_rooms(rooms, sample) {
            nearest = last_clear_distance.min(nearest);
            break;
        }
        if let Some(hit) = nearest_wall_hit_around_rooms(
            rooms,
            from,
            to,
            max_distance,
            sample,
            config,
            &mut checked_cells,
        ) {
            nearest = hit.min(nearest);
            break;
        }
        last_clear_distance = (max_distance.saturating_mul(i)) / steps;
        i += 1;
    }

    if last_clear_distance == max_distance {
        max_distance
    } else {
        nearest
            .saturating_sub(config.collision_margin)
            .clamp(0, config.distance)
    }
}

fn first_collision_room_sector_size(rooms: &[CharacterCollisionRoom<'_>]) -> Option<i32> {
    let mut i = 0usize;
    while i < rooms.len() && i < MAX_CAMERA_COLLISION_ROOMS {
        if let Some(room) = rooms[i].room {
            return Some(room.collision().sector_size().max(1));
        }
        i += 1;
    }
    None
}

fn point_outside_camera_space(
    room: RoomCollision<'_, '_>,
    point: RoomPoint,
    sector_size: i32,
    room_width: i32,
    room_depth: i32,
) -> bool {
    if point.x < 0 || point.z < 0 {
        return true;
    }
    let sx = point.x / sector_size;
    let sz = point.z / sector_size;
    if sx < 0 || sz < 0 || sx >= room_width || sz >= room_depth {
        return true;
    }
    match room.sector_probe(sx as u16, sz as u16) {
        Some(sector) => !sector.has_floor(),
        None => true,
    }
}

fn point_outside_camera_rooms(rooms: &[CharacterCollisionRoom<'_>], point: RoomPoint) -> bool {
    let mut i = 0usize;
    while i < rooms.len() && i < MAX_CAMERA_COLLISION_ROOMS {
        let entry = rooms[i];
        if let Some(room) = entry.room {
            let collision = room.collision();
            let local = collision_room_local_point(entry, point);
            if !point_outside_camera_space(
                collision,
                local,
                collision.sector_size().max(1),
                collision.width() as i32,
                collision.depth() as i32,
            ) {
                return false;
            }
        }
        i += 1;
    }
    true
}

fn nearest_wall_hit_around(
    room: RoomCollision<'_, '_>,
    sample: RoomPoint,
    ray: CameraRay,
    checked_cells: &mut CheckedCameraCells,
) -> Option<i32> {
    if sample.x < 0 || sample.z < 0 {
        return None;
    }
    let sx = sample.x / ray.sector_size;
    let sz = sample.z / ray.sector_size;
    let mut nearest: Option<i32> = None;
    let mut ox = -1;
    while ox <= 1 {
        let mut oz = -1;
        while oz <= 1 {
            let cx = sx + ox;
            let cz = sz + oz;
            if cx >= 0 && cz >= 0 && cx < ray.room_width && cz < ray.room_depth {
                let key = (cx as u32)
                    .saturating_mul(ray.room_depth as u32)
                    .saturating_add(cz as u32);
                if !checked_cells.visit(key) {
                    oz += 1;
                    continue;
                }
                if let Some(sector) = room.sector_probe(cx as u16, cz as u16) {
                    let mut i = 0;
                    while i < sector.wall_count() {
                        if let Some(wall) = room.sector_probe_wall(sector, i) {
                            if wall.solid() {
                                if let Some(hit) = segment_wall_hit_distance(
                                    ray,
                                    cx,
                                    cz,
                                    wall.direction(),
                                    wall.heights(),
                                ) {
                                    nearest = Some(match nearest {
                                        Some(prev) => prev.min(hit),
                                        None => hit,
                                    });
                                }
                            }
                        }
                        i += 1;
                    }
                }
            }
            oz += 1;
        }
        ox += 1;
    }
    nearest
}

fn nearest_wall_hit_around_rooms(
    rooms: &[CharacterCollisionRoom<'_>],
    from: RoomPoint,
    to: RoomPoint,
    max_distance: i32,
    sample: RoomPoint,
    config: ThirdPersonCameraConfig,
    checked_cells: &mut [CheckedCameraCells; MAX_CAMERA_COLLISION_ROOMS],
) -> Option<i32> {
    let mut nearest: Option<i32> = None;
    let mut i = 0usize;
    while i < rooms.len() && i < MAX_CAMERA_COLLISION_ROOMS {
        let entry = rooms[i];
        if let Some(room) = entry.room {
            let collision = room.collision();
            let local_from = collision_room_local_point(entry, from);
            let local_to = collision_room_local_point(entry, to);
            let local_sample = collision_room_local_point(entry, sample);
            let ray = CameraRay {
                from: local_from,
                to: local_to,
                dx: local_to.x.saturating_sub(local_from.x),
                dy: local_to.y.saturating_sub(local_from.y),
                dz: local_to.z.saturating_sub(local_from.z),
                distance: max_distance,
                sector_size: collision.sector_size().max(1),
                room_width: collision.width() as i32,
                room_depth: collision.depth() as i32,
                vertical_margin: config.collision_margin,
            };
            if let Some(hit) =
                nearest_wall_hit_around(collision, local_sample, ray, &mut checked_cells[i])
            {
                nearest = Some(match nearest {
                    Some(prev) => prev.min(hit),
                    None => hit,
                });
            }
        }
        i += 1;
    }
    nearest
}

fn segment_wall_hit_distance(
    ray: CameraRay,
    sx: i32,
    sz: i32,
    direction: u8,
    heights: [i32; 4],
) -> Option<i32> {
    if ray.distance <= 0 {
        return None;
    }
    let sector_size = ray.sector_size;
    let x0 = sx.saturating_mul(sector_size);
    let x1 = x0.saturating_add(sector_size);
    let z0 = sz.saturating_mul(sector_size);
    let z1 = z0.saturating_add(sector_size);
    let diagonal_axis_q12 = match direction {
        DIR_NORTH_WEST_SOUTH_EAST => {
            intersect_segment_q12(ray.from.x, ray.from.z, ray.dx, ray.dz, x0, z0, x1, z1)
        }
        DIR_NORTH_EAST_SOUTH_WEST => {
            intersect_segment_q12(ray.from.x, ray.from.z, ray.dx, ray.dz, x1, z0, x0, z1)
        }
        _ => None,
    };
    let t_q12 = match direction {
        DIR_NORTH => intersect_horizontal_q12(ray.from.z, ray.dz, z0),
        DIR_SOUTH => intersect_horizontal_q12(ray.from.z, ray.dz, z1),
        DIR_EAST => intersect_vertical_q12(ray.from.x, ray.dx, x1),
        DIR_WEST => intersect_vertical_q12(ray.from.x, ray.dx, x0),
        DIR_NORTH_WEST_SOUTH_EAST | DIR_NORTH_EAST_SOUTH_WEST => diagonal_axis_q12.map(|(t, _)| t),
        _ => None,
    }?;
    if !(0..=Q12::SCALE).contains(&t_q12) {
        return None;
    }
    let t = Q12::from_raw(t_q12);
    let x_at = ray.from.x.saturating_add(t.mul_i32(ray.dx));
    let y_at = ray.from.y.saturating_add(t.mul_i32(ray.dy));
    let z_at = ray.from.z.saturating_add(t.mul_i32(ray.dz));
    let wall_axis_q12 = match direction {
        DIR_NORTH | DIR_SOUTH => {
            if x_at < x0 || x_at > x1 {
                return None;
            }
            (x_at.saturating_sub(x0))
                .saturating_mul(Q12::SCALE)
                .checked_div(sector_size.max(1))?
        }
        DIR_EAST | DIR_WEST => {
            if z_at < z0 || z_at > z1 {
                return None;
            }
            (z_at.saturating_sub(z0))
                .saturating_mul(Q12::SCALE)
                .checked_div(sector_size.max(1))?
        }
        DIR_NORTH_WEST_SOUTH_EAST | DIR_NORTH_EAST_SOUTH_WEST => diagonal_axis_q12?.1,
        _ => return None,
    };
    let axis = Q12::from_raw(wall_axis_q12.clamp(0, Q12::SCALE));
    let (bottom, top) = match direction {
        DIR_NORTH | DIR_EAST | DIR_NORTH_WEST_SOUTH_EAST | DIR_NORTH_EAST_SOUTH_WEST => (
            lerp_i32(heights[0], heights[1], axis),
            lerp_i32(heights[3], heights[2], axis),
        ),
        DIR_SOUTH | DIR_WEST => (
            lerp_i32(heights[1], heights[0], axis),
            lerp_i32(heights[2], heights[3], axis),
        ),
        _ => return None,
    };
    let min_y = bottom.min(top).saturating_sub(ray.vertical_margin);
    let max_y = bottom.max(top).saturating_add(ray.vertical_margin);
    if y_at < min_y || y_at > max_y {
        return None;
    }
    Some(t.mul_i32(ray.distance))
}

fn intersect_segment_q12(
    from_x: i32,
    from_z: i32,
    dx: i32,
    dz: i32,
    ax: i32,
    az: i32,
    bx: i32,
    bz: i32,
) -> Option<(i32, i32)> {
    let sx = bx.saturating_sub(ax);
    let sz = bz.saturating_sub(az);
    let qx = ax.saturating_sub(from_x);
    let qz = az.saturating_sub(from_z);
    let denom = cross_i32(dx, dz, sx, sz);
    if denom == 0 {
        return None;
    }
    let t_num = cross_i32(qx, qz, sx, sz);
    let u_num = cross_i32(qx, qz, dx, dz);
    let t_q12 = div_q12_signed(t_num, denom)?;
    let u_q12 = div_q12_signed(u_num, denom)?;
    if !(0..=Q12::SCALE).contains(&t_q12) || !(0..=Q12::SCALE).contains(&u_q12) {
        return None;
    }
    Some((t_q12, u_q12))
}

fn cross_i32(ax: i32, az: i32, bx: i32, bz: i32) -> i32 {
    ax.saturating_mul(bz).saturating_sub(az.saturating_mul(bx))
}

fn div_q12_signed(num: i32, denom: i32) -> Option<i32> {
    if denom == 0 {
        None
    } else {
        Some(div_q12_i32(num, denom))
    }
}

fn intersect_horizontal_q12(from_z: i32, dz: i32, wall_z: i32) -> Option<i32> {
    if dz == 0 {
        return None;
    }
    let delta = wall_z.saturating_sub(from_z);
    if !delta_within_segment(delta, dz) {
        return None;
    }
    delta.saturating_mul(Q12::SCALE).checked_div(dz)
}

fn intersect_vertical_q12(from_x: i32, dx: i32, wall_x: i32) -> Option<i32> {
    if dx == 0 {
        return None;
    }
    let delta = wall_x.saturating_sub(from_x);
    if !delta_within_segment(delta, dx) {
        return None;
    }
    delta.saturating_mul(Q12::SCALE).checked_div(dx)
}

fn delta_within_segment(delta: i32, axis_delta: i32) -> bool {
    if axis_delta > 0 {
        delta >= 0 && delta <= axis_delta
    } else {
        delta <= 0 && delta >= axis_delta
    }
}

fn camera_position(focus: RoomPoint, distance: i32, yaw: Angle, pitch_q12: i16) -> RoomPoint {
    let sin_yaw = yaw.sin();
    let cos_yaw = yaw.cos();
    let pitch = signed_q12_angle(pitch_q12);
    let sin_pitch = pitch.sin();
    let cos_pitch = pitch.cos();
    let horizontal = cos_pitch.mul_i32(distance);
    RoomPoint::new(
        focus.x.saturating_add(sin_yaw.mul_i32(horizontal)),
        focus.y.saturating_add(sin_pitch.mul_i32(distance)),
        focus.z.saturating_add(cos_yaw.mul_i32(horizontal)),
    )
}

fn camera_position_at_height(
    focus: RoomPoint,
    distance: i32,
    yaw: Angle,
    pitch_q12: i16,
    camera_y: i32,
) -> RoomPoint {
    let orbit = camera_position(focus, distance, yaw, pitch_q12);
    RoomPoint::new(orbit.x, camera_y, orbit.z)
}

fn camera_height_goal(player: RoomPoint, pitch_q12: i16, config: ThirdPersonCameraConfig) -> i32 {
    let authored_pitch = default_pitch_q12(config);
    let manual_height_delta = pitch_vertical_offset(config.distance, pitch_q12)
        .saturating_sub(pitch_vertical_offset(config.distance, authored_pitch));
    player
        .y
        .saturating_add(config.height)
        .saturating_add(manual_height_delta)
}

fn pitch_vertical_offset(distance: i32, pitch_q12: i16) -> i32 {
    signed_q12_angle(pitch_q12).sin().mul_i32(distance)
}

fn camera_from_position_focus(
    projection: WorldProjection,
    position: RoomPoint,
    focus: RoomPoint,
    fallback_yaw: Angle,
) -> WorldCamera {
    let dx = position.x.saturating_sub(focus.x);
    let dz = position.z.saturating_sub(focus.z);
    let radius = isqrt_i32(dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz))).max(1);
    let target_dy = focus.y.saturating_sub(position.y);
    let pitch_len = isqrt_i32(
        radius
            .saturating_mul(radius)
            .saturating_add(target_dy.saturating_mul(target_dy)),
    )
    .max(1);
    // A wall can shorten the arm to zero. Keep an orthonormal view at that
    // position instead of producing a zero yaw basis and a blank world.
    let (sin_yaw, cos_yaw) = if dx == 0 && dz == 0 {
        (fallback_yaw.sin(), fallback_yaw.cos())
    } else {
        (Q12::from_ratio(dx, radius), Q12::from_ratio(dz, radius))
    };
    WorldCamera {
        position,
        projection,
        sin_yaw,
        cos_yaw,
        sin_pitch: Q12::from_ratio(target_dy, pitch_len),
        cos_pitch: Q12::from_ratio(radius, pitch_len),
    }
}

fn default_pitch_q12(config: ThirdPersonCameraConfig) -> i16 {
    let vertical = config.height.saturating_sub(config.target_height);
    let pitch = if config.lock_target_framing {
        framing::pitch_from_height_offset(vertical, config.distance)
    } else {
        pitch_from_vertical_distance(vertical, config.distance)
    };
    pitch.clamp(config.pitch_min_q12, config.pitch_max_q12)
}

fn lock_pitch_offset_q12(config: ThirdPersonCameraConfig, height_offset: i32) -> i16 {
    let base = default_pitch_q12(config);
    let raised = pitch_from_vertical_distance(
        config
            .height
            .saturating_sub(config.target_height)
            .saturating_add(height_offset),
        config.distance,
    )
    .clamp(config.pitch_min_q12, config.pitch_max_q12);
    raised.saturating_sub(base).max(0)
}

fn pitch_from_vertical_distance(vertical: i32, horizontal: i32) -> i16 {
    if vertical == 0 {
        return 0;
    }
    let ay = abs_i32(vertical);
    let ax = abs_i32(horizontal).max(1);
    let base = if ay <= ax {
        ay.saturating_mul(512) / ax
    } else {
        1024 - (ax.saturating_mul(512) / ay.max(1))
    }
    .min(1024);
    let signed = if vertical < 0 { -base } else { base };
    signed.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn signed_q12_angle(q12: i16) -> Angle {
    Angle::from_q12(((q12 as i32) & 0x0FFF) as u16)
}

fn lerp_i32(a: i32, b: i32, t: Q12) -> i32 {
    a.saturating_add(t.mul_i32(b.saturating_sub(a)))
}

fn yaw_to_point(from: RoomPoint, to: RoomPoint) -> Angle {
    let dx = to.x.saturating_sub(from.x);
    let dz = to.z.saturating_sub(from.z);
    if dx == 0 && dz == 0 {
        return Angle::ZERO;
    }
    let ax = abs_i32(dx);
    let az = abs_i32(dz);
    let base = if ax <= az {
        ax.saturating_mul(512) / az.max(1)
    } else {
        1024 - (az.saturating_mul(512) / ax.max(1))
    };
    let angle = if dz >= 0 {
        if dx >= 0 {
            base
        } else {
            4096 - base
        }
    } else if dx >= 0 {
        2048 - base
    } else {
        2048 + base
    };
    Angle::from_q12((angle & 0x0FFF) as u16)
}

fn approach_i16(current: i16, target: i16, step: i16) -> i16 {
    let step = step.max(1);
    let delta = target.saturating_sub(current);
    if abs_i16(delta) <= step {
        target
    } else if delta > 0 {
        current.saturating_add(step)
    } else {
        current.saturating_sub(step)
    }
}

fn approach_i32_shift(current: i32, target: i32, shift: u8) -> i32 {
    if current == target {
        return current;
    }
    let shift = shift.min(6);
    let delta = target.saturating_sub(current);
    let step = if shift == 0 { delta } else { delta >> shift };
    if step == 0 {
        current.saturating_add(delta.signum())
    } else {
        current.saturating_add(step)
    }
}

fn approach_vertex_shift(
    current: RoomPoint,
    target: RoomPoint,
    shift: u8,
    vertical_shift: u8,
) -> RoomPoint {
    RoomPoint::new(
        approach_i32_shift(current.x, target.x, shift),
        approach_i32_shift(current.y, target.y, vertical_shift),
        approach_i32_shift(current.z, target.z, shift),
    )
}

fn lerp_vertex(from: RoomPoint, to: RoomPoint, num: i32, den: i32) -> RoomPoint {
    RoomPoint::new(
        from.x + ((to.x - from.x) * num) / den,
        from.y + ((to.y - from.y) * num) / den,
        from.z + ((to.z - from.z) * num) / den,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CharacterBlockerTraceProvider, CharacterCollisionAabb, RuntimeRoom};

    struct ClearTraceProvider;

    impl CollisionTraceProvider for ClearTraceProvider {
        fn trace_into(
            &mut self,
            query: CollisionTraceQuery,
            output: &mut crate::CollisionTrace,
        ) -> bool {
            *output = crate::CollisionTrace::unobstructed(query.end);
            true
        }
    }

    struct HalfDistanceTraceProvider {
        fail: bool,
        calls: u8,
    }

    impl CollisionTraceProvider for HalfDistanceTraceProvider {
        fn trace_into(
            &mut self,
            query: CollisionTraceQuery,
            output: &mut crate::CollisionTrace,
        ) -> bool {
            self.calls = self.calls.saturating_add(1);
            if self.fail {
                return false;
            }
            let mut trace = crate::CollisionTrace::unobstructed(query.end);
            trace.fraction_q12 = COLLISION_FRACTION_ONE_Q12 / 2;
            trace.end = RoomPoint::new(
                query.start.x + (query.end.x - query.start.x) / 2,
                query.start.y + (query.end.y - query.start.y) / 2,
                query.start.z + (query.end.z - query.start.z) / 2,
            );
            *output = trace;
            true
        }
    }

    struct CloseDistanceTraceProvider;

    impl CollisionTraceProvider for CloseDistanceTraceProvider {
        fn trace_into(
            &mut self,
            query: CollisionTraceQuery,
            output: &mut crate::CollisionTrace,
        ) -> bool {
            let mut trace = crate::CollisionTrace::unobstructed(query.end);
            trace.fraction_q12 = COLLISION_FRACTION_ONE_Q12 / 64;
            trace.end = RoomPoint::new(
                query.start.x + (query.end.x - query.start.x) / 64,
                query.start.y + (query.end.y - query.start.y) / 64,
                query.start.z + (query.end.z - query.start.z) / 64,
            );
            *output = trace;
            true
        }
    }

    /// A wall 8 units behind (-z) the start of every trace. Camera traces
    /// start at the focus, so this is a wall flush against the player's back,
    /// the way Aletha stands after dashing along a pillar.
    struct WallBehindTraceProvider;

    const WALL_GAP: i32 = 8;

    impl CollisionTraceProvider for WallBehindTraceProvider {
        fn trace_into(
            &mut self,
            query: CollisionTraceQuery,
            output: &mut crate::CollisionTrace,
        ) -> bool {
            let mut trace = crate::CollisionTrace::unobstructed(query.end);
            let wall = query.start.z - WALL_GAP;
            if query.end.z < wall {
                let span = query.start.z - query.end.z;
                trace.fraction_q12 = WALL_GAP * COLLISION_FRACTION_ONE_Q12 / span;
                trace.end = RoomPoint::new(
                    query.start.x + (query.end.x - query.start.x) * WALL_GAP / span,
                    query.start.y + (query.end.y - query.start.y) * WALL_GAP / span,
                    wall,
                );
            }
            *output = trace;
            true
        }
    }

    #[test]
    fn arm_blocked_inside_the_body_swings_to_a_clear_side_and_holds_it() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut config = ThirdPersonCameraConfig::character(400, 100, 50);
        config.collision_margin = 12;
        // Locked on an enemy straight ahead (+z), so lock-on keeps steering
        // the camera to straight behind (-z), which is inside the wall.
        let target = ThirdPersonCameraTarget {
            lock_target: Some(RoomPoint::new(0, 0, 1000)),
            ..trace_target()
        };
        let behind = yaw_to_point(target.player, RoomPoint::new(0, 0, 1000)).add(Angle::HALF);
        assert!(camera_position(RoomPoint::ZERO, 100, behind, 0).z < 0);
        let blocked = solve_camera_collision_trace(
            &mut WallBehindTraceProvider,
            RoomPoint::new(0, 50, 0),
            behind,
            0,
            100,
            config,
        )
        .unwrap();
        assert!(blocked.distance < config.min_distance, "{blocked:?}");

        let mut camera = ThirdPersonCameraState::new(behind);
        camera.snap_to_player_with_yaw(target, config, behind);
        let mut yaws = [Angle::ZERO; 8];
        for yaw in &mut yaws {
            let frame = camera
                .update_vblanks_with_trace_provider(
                    projection,
                    &mut WallBehindTraceProvider,
                    target,
                    ThirdPersonCameraInput::default(),
                    config,
                    1,
                )
                .expect("wall camera update");
            assert!(frame.distance >= config.distance / 2, "{frame:?}");
            let (eye, focus) = (camera.position(), camera.focus());
            assert!(eye.z >= focus.z - WALL_GAP, "{eye:?} behind {focus:?}");
            *yaw = camera.yaw();
        }
        // Swung clear once, then held: lock-on steering does not drag it back
        // into the wall and bounce it out again.
        assert_ne!(yaws[0], behind);
        assert!(yaws.iter().all(|&yaw| yaw == yaws[0]), "{yaws:?}");
    }

    #[test]
    fn free_camera_blocked_inside_the_body_keeps_its_yaw() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut config = ThirdPersonCameraConfig::character(400, 100, 50);
        config.collision_margin = 12;
        let behind = Angle::HALF;
        let update = |moving: bool| {
            let target = ThirdPersonCameraTarget {
                moving,
                ..trace_target()
            };
            let mut camera = ThirdPersonCameraState::new(behind);
            camera.snap_to_player_with_yaw(target, config, behind);
            let frame = camera
                .update_vblanks_with_trace_provider(
                    projection,
                    &mut WallBehindTraceProvider,
                    target,
                    ThirdPersonCameraInput::default(),
                    config,
                    1,
                )
                .expect("wall camera update");
            (camera, frame)
        };
        // Running or standing, no lock-on: the stick directions stay put and
        // the arm collapses; the scene hides the player below min_distance.
        for moving in [true, false] {
            let (camera, frame) = update(moving);
            assert_eq!(camera.yaw(), behind);
            assert!(frame.distance < config.min_distance, "{frame:?}");
            assert_eq!(camera.distance(), frame.distance);
        }
    }

    // A tight corner with a narrow blocked bearing and equally usable side
    // views. The partially clear version cannot fit a full camera boom.
    struct CornerOrbitBackend {
        side_distance: i32,
        blocked: bool,
    }
    impl CameraCollisionBackend for CornerOrbitBackend {
        fn constrain_segment(
            &mut self,
            _start: RoomPoint,
            end: RoomPoint,
            _config: ThirdPersonCameraConfig,
        ) -> Result<RoomPoint, CollisionQueryError> {
            Ok(end)
        }
        fn clamp_to_floor(
            &mut self,
            p: RoomPoint,
            _clearance: i32,
        ) -> Result<RoomPoint, CollisionQueryError> {
            Ok(p)
        }
        fn solve(
            &mut self,
            _focus: RoomPoint,
            yaw: Angle,
            _pitch: i16,
            _height: i32,
            config: ThirdPersonCameraConfig,
        ) -> Result<CollisionSolve, CollisionQueryError> {
            let distance = if !self.blocked {
                config.distance
            } else if yaw.shortest_delta_q12(Angle::HALF).unsigned_abs() < 128 {
                4
            } else {
                self.side_distance
            };
            Ok(CollisionSolve {
                distance,
                pull_in: distance < config.distance,
            })
        }
    }

    #[test]
    fn locked_corner_holds_one_clear_side_until_target_bearing_reopens() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let config = ThirdPersonCameraConfig::character(400, 100, 50);
        let target = ThirdPersonCameraTarget {
            lock_target: Some(RoomPoint::new(0, 0, 1000)),
            ..trace_target()
        };
        for side_distance in [config.min_distance + 6, config.distance] {
            let mut backend = CornerOrbitBackend {
                side_distance,
                blocked: true,
            };
            let mut camera = ThirdPersonCameraState::new(Angle::HALF);
            camera.snap_to_player_with_yaw(target, config, Angle::HALF);
            let first = camera
                .update_vblanks_with_backend(
                    projection,
                    &mut backend,
                    target,
                    ThirdPersonCameraInput::default(),
                    config,
                    1,
                )
                .unwrap();
            assert_ne!(first.yaw, Angle::HALF);
            for _ in 0..120 {
                let frame = camera
                    .update_vblanks_with_backend(
                        projection,
                        &mut backend,
                        target,
                        ThirdPersonCameraInput::default(),
                        config,
                        1,
                    )
                    .unwrap();
                assert_eq!(
                    frame.yaw, first.yaw,
                    "no orbit cycling with side distance {side_distance}"
                );
                assert!(frame.distance >= side_distance);
            }
            backend.blocked = false;
            let mut last = camera.yaw();
            for _ in 0..120 {
                let frame = camera
                    .update_vblanks_with_backend(
                        projection,
                        &mut backend,
                        target,
                        ThirdPersonCameraInput::default(),
                        config,
                        1,
                    )
                    .unwrap();
                assert!(
                    last.shortest_delta_q12(frame.yaw).unsigned_abs()
                        <= config.lock_on_align_step.as_q12()
                );
                last = frame.yaw;
            }
            assert_eq!(camera.yaw(), Angle::HALF);
            assert!(!camera.clear_orbit_hold);
        }
    }

    fn trace_target() -> ThirdPersonCameraTarget {
        ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        }
    }

    #[test]
    fn clear_trace_keeps_full_boom_and_allows_position_smoothing() {
        let config = ThirdPersonCameraConfig::character(1400, 700, 0);
        let solve = solve_camera_collision_trace(
            &mut ClearTraceProvider,
            RoomPoint::ZERO,
            Angle::HALF,
            0,
            700,
            config,
        )
        .unwrap();
        assert_eq!(solve.distance, 1400);
        assert!(!solve.pull_in);
    }

    #[test]
    fn constrained_long_segment_does_not_overflow() {
        let start = RoomPoint::new(-50000, 0, 0);
        let end = RoomPoint::new(50000, 0, 0);
        assert_eq!(
            lerp_clear_segment(start, end, 50000, 100000),
            RoomPoint::ZERO
        );
        assert_eq!(lerp_clear_segment(start, end, 100000, 100000), end);
    }

    #[test]
    fn trace_provider_shortens_camera_spring_arm() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut provider = HalfDistanceTraceProvider {
            fail: false,
            calls: 0,
        };
        let frame = camera
            .update_vblanks_with_trace_provider(
                WorldProjection::new(160, 120, 320, 64),
                &mut provider,
                trace_target(),
                ThirdPersonCameraInput::default(),
                ThirdPersonCameraConfig::character(1400, 700, 0),
                1,
            )
            .expect("trace camera update");
        assert!(frame.collision_pull_in);
        assert_eq!(frame.distance, 690);
        assert_eq!(provider.calls, 1);
    }

    #[test]
    fn close_obstruction_overrides_preferred_minimum_distance() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.collision_margin = 0;
        assert_eq!(config.min_distance, 24);

        let frame = camera
            .update_vblanks_with_trace_provider(
                WorldProjection::new(160, 120, 320, 64),
                &mut CloseDistanceTraceProvider,
                trace_target(),
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .expect("trace camera update");

        assert!(frame.collision_pull_in);
        assert_eq!(frame.distance, 21);
        assert!(frame.distance < config.min_distance);
    }

    #[test]
    fn collidable_prop_aabb_shortens_camera_spring_arm() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let target = trace_target();
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.collision_margin = 0;

        let mut clear_camera = ThirdPersonCameraState::new(Angle::HALF);
        clear_camera.snap_to_player_with_yaw(target, config, Angle::HALF);
        let clear = clear_camera
            .update_vblanks_with_trace_provider(
                projection,
                &mut ClearTraceProvider,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .expect("clear camera update");
        assert!(!clear.collision_pull_in);

        let blockers = [CharacterCollisionAabb::new(
            RoomPoint::new(-64, 1, -800),
            RoomPoint::new(64, 1_000, -600),
        )];
        let mut clear_world = ClearTraceProvider;
        let mut props =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut clear_world, &[], &blockers);
        let mut blocked_camera = ThirdPersonCameraState::new(Angle::HALF);
        blocked_camera.snap_to_player_with_yaw(target, config, Angle::HALF);
        let blocked = blocked_camera
            .update_vblanks_with_trace_provider(
                projection,
                &mut props,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .expect("prop-blocked camera update");
        assert!(blocked.collision_pull_in);
        assert!(blocked.distance < clear.distance);
    }

    #[test]
    fn lock_focus_cannot_cross_a_pillar_before_the_arm_sweep() {
        let mut config = ThirdPersonCameraConfig::character(1400, 500, 400);
        config.focus_lag_shift = 0;
        let mut target = trace_target();
        target.lock_target = Some(RoomPoint::new(1000, 0, 0));
        let blockers = [CharacterCollisionAabb::new(
            RoomPoint::new(80, 0, -200),
            RoomPoint::new(300, 1000, 200),
        )];
        let mut world = ClearTraceProvider;
        let mut props = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &blockers);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        for _ in 0..120 {
            let frame = camera
                .update_vblanks_with_trace_provider(
                    WorldProjection::new(160, 120, 320, 64),
                    &mut props,
                    target,
                    ThirdPersonCameraInput::default(),
                    config,
                    1,
                )
                .unwrap();
            assert!(
                frame.focus.x < 80,
                "focus entered the pillar: {:?}",
                frame.focus
            );
            assert!(frame.distance > 0, "solid focus collapsed the camera arm");
        }
    }

    #[test]
    fn collapsed_arm_retains_a_valid_view_orientation() {
        for yaw in [Angle::ZERO, Angle::QUARTER, Angle::HALF] {
            let position = RoomPoint::new(938, 297, -1216);
            let view = camera_from_position_focus(
                WorldProjection::new(160, 120, 320, 64),
                position,
                position,
                yaw,
            );
            assert_eq!(view.position, position);
            assert_eq!(view.sin_yaw, yaw.sin());
            assert_eq!(view.cos_yaw, yaw.cos());
            assert_eq!(view.cos_pitch, Q12::from_ratio(1, 1));
        }
    }

    #[test]
    fn low_ceiling_cannot_contain_the_camera_focus() {
        let mut config = ThirdPersonCameraConfig::character(400, 120, 100);
        config.focus_lag_shift = 0;
        config.collision_margin = 4;
        let blockers = [CharacterCollisionAabb::new(
            RoomPoint::new(-1000, 80, -1000),
            RoomPoint::new(1000, 200, 1000),
        )];
        let mut world = ClearTraceProvider;
        let mut props = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &blockers);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        for _ in 0..30 {
            let frame = camera
                .update_vblanks_with_trace_provider(
                    WorldProjection::new(160, 120, 320, 64),
                    &mut props,
                    trace_target(),
                    ThirdPersonCameraInput::default(),
                    config,
                    1,
                )
                .unwrap();
            assert!(frame.focus.y < 80);
            assert!(frame.distance > 0);
            assert!(frame.camera.position.y < 80);
        }
    }

    #[test]
    fn orbit_easing_cannot_cut_through_a_clear_goals_corner() {
        let mut config = ThirdPersonCameraConfig::character(500, 400, 400);
        config.position_lag_shift = 3;
        config.lock_height_boost = 0;
        let target = trace_target();
        let blockers = [CharacterCollisionAabb::new(
            RoomPoint::new(60, 0, -180),
            RoomPoint::new(100, 1000, -20),
        )];
        let mut world = ClearTraceProvider;
        let mut props = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &blockers);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        camera.snap_to_player_with_yaw(target, config, Angle::HALF);
        camera.position = RoomPoint::new(200, 400, -100);
        let frame = camera
            .update_vblanks_with_trace_provider(
                WorldProjection::new(160, 120, 320, 64),
                &mut props,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .unwrap();
        let visibility = trace_collision(
            &mut props,
            CollisionTraceQuery::point(frame.focus, frame.camera.position),
        )
        .unwrap();
        assert!(
            !visibility.hit(),
            "smoothed camera crossed the pillar: {:?}",
            frame.camera.position
        );
        assert!(frame.camera.position.x < 60);
    }

    #[test]
    fn cached_shortened_arm_rechecks_the_ray_after_player_translation() {
        let mut config = ThirdPersonCameraConfig::character(1000, 400, 400);
        config.collision_solve_interval = 2;
        config.focus_lag_shift = 0;
        config.lock_height_boost = 0;
        let blockers = [
            CharacterCollisionAabb::new(
                RoomPoint::new(-1000, 0, -620),
                RoomPoint::new(1000, 1000, -600),
            ),
            CharacterCollisionAabb::new(
                RoomPoint::new(80, 0, -400),
                RoomPoint::new(120, 1000, -300),
            ),
        ];
        let mut world = ClearTraceProvider;
        let mut props = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &blockers);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut target = trace_target();
        let projection = WorldProjection::new(160, 120, 320, 64);
        let first = camera
            .update_vblanks_with_trace_provider(
                projection,
                &mut props,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .unwrap();
        assert!(first.camera.position.z < -500);
        assert_eq!(camera.solve_phase, 1);
        target.player.x = 100;
        let moved = camera
            .update_vblanks_with_trace_provider(
                projection,
                &mut props,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .unwrap();
        let ray = trace_collision(
            &mut props,
            CollisionTraceQuery::point(moved.focus, moved.camera.position),
        )
        .unwrap();
        assert!(
            !ray.hit(),
            "cached boom passed through a nearer wall after moving sideways"
        );
        assert!(moved.camera.position.z > -300);
    }

    #[test]
    fn steep_pitch_corner_clamp_retains_boom_length_not_horizontal_radius() {
        let mut config = ThirdPersonCameraConfig::character(500, 400, 400);
        config.position_lag_shift = 3;
        config.lock_height_boost = 0;
        config.pitch_min_q12 = 682;
        config.pitch_max_q12 = 682;
        let target = trace_target();
        let blockers = [CharacterCollisionAabb::new(
            RoomPoint::new(60, 0, -180),
            RoomPoint::new(100, 1000, -20),
        )];
        let mut world = ClearTraceProvider;
        let mut props = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &blockers);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        camera.snap_to_player_with_yaw(target, config, Angle::HALF);
        camera.position = RoomPoint::new(200, 400, -100);
        let frame = camera
            .update_vblanks_with_trace_provider(
                WorldProjection::new(160, 120, 320, 64),
                &mut props,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .unwrap();
        assert!(frame.collision_pull_in);
        let dx = frame.camera.position.x - frame.focus.x;
        let dz = frame.camera.position.z - frame.focus.z;
        let horizontal = isqrt_i32(dx * dx + dz * dz);
        assert!(horizontal > 0);
        assert!(frame.distance > horizontal * 18 / 10,
            "60-degree boom stored horizontal radius and would shorten twice: distance={}, horizontal={}", frame.distance, horizontal);
    }

    #[test]
    fn floor_lift_cannot_push_a_shortened_arm_into_an_overhang() {
        let mut config = ThirdPersonCameraConfig::character(1000, 3, 3);
        config.min_floor_clearance = 64;
        config.pitch_min_q12 = 0;
        config.pitch_max_q12 = 0;
        let blockers = [
            CharacterCollisionAabb::new(
                RoomPoint::new(-1024, -2, -1024),
                RoomPoint::new(1024, 2, 1024),
            ),
            CharacterCollisionAabb::new(
                RoomPoint::new(-1024, 0, -800),
                RoomPoint::new(1024, 1000, -600),
            ),
            CharacterCollisionAabb::new(
                RoomPoint::new(-1024, 40, -600),
                RoomPoint::new(1024, 100, -200),
            ),
        ];
        let mut world = ClearTraceProvider;
        let mut props = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &blockers);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let frame = camera
            .update_vblanks_with_trace_provider(
                WorldProjection::new(160, 120, 320, 64),
                &mut props,
                trace_target(),
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .unwrap();
        assert!(frame.collision_pull_in);
        let visibility = trace_collision(
            &mut props,
            CollisionTraceQuery::point(frame.focus, frame.camera.position),
        )
        .unwrap();
        assert!(
            !visibility.hit(),
            "floor clearance put the camera in the overhang"
        );
        assert!(frame.camera.position.y > 2);
    }

    #[test]
    fn collidable_prop_floor_clamps_the_full_trace_camera_update() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let target = trace_target();
        let mut config = ThirdPersonCameraConfig::character(384, 3, 3);
        config.collision_margin = 0;
        config.min_floor_clearance = 64;
        config.pitch_min_q12 = 0;
        config.pitch_max_q12 = 0;
        let floor = [CharacterCollisionAabb::new(
            RoomPoint::new(-1_024, -2, -1_024),
            RoomPoint::new(1_024, 2, 1_024),
        )];
        let mut clear_world = ClearTraceProvider;
        let mut props =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut clear_world, &[], &floor);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        camera.snap_to_player_with_yaw(target, config, Angle::HALF);

        let frame = camera
            .update_vblanks_with_trace_provider(
                projection,
                &mut props,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .expect("prop floor camera update");

        assert!(!frame.collision_pull_in);
        assert_eq!(frame.camera.position.y, 68);
        assert!(
            frame.camera.position.y >= floor[0].max.y + config.min_floor_clearance,
            "conservative sub-sample contact must not leave the camera below clearance"
        );
    }

    #[test]
    fn malformed_and_overflow_prop_state_roll_back_the_full_camera_update() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let target = trace_target();
        let config = ThirdPersonCameraConfig::character(1_400, 700, 0);
        let malformed = [CharacterCollisionAabb::new(
            RoomPoint::new(64, 8, 64),
            RoomPoint::new(-64, 0, -64),
        )];
        let mut clear_world = ClearTraceProvider;
        let mut props =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut clear_world, &[], &malformed);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let before = camera;
        assert_eq!(
            camera.update_vblanks_with_trace_provider(
                projection,
                &mut props,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            ),
            Err(CollisionQueryError)
        );
        assert_eq!(camera, before);

        let valid = CharacterCollisionAabb::new(
            RoomPoint::new(-64, 1, -800),
            RoomPoint::new(64, 1_000, -600),
        );
        let overflow = [valid; psx_level::MAX_STATIC_PROP_AABB_BLOCKERS + 1];
        let mut props =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut clear_world, &[], &overflow);
        assert_eq!(
            camera.update_vblanks_with_trace_provider(
                projection,
                &mut props,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            ),
            Err(CollisionQueryError)
        );
        assert_eq!(camera, before);
    }

    #[test]
    fn trace_provider_failure_rolls_back_complete_camera_state() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let before = camera;
        let mut provider = HalfDistanceTraceProvider {
            fail: true,
            calls: 0,
        };
        let result = camera.update_vblanks_with_trace_provider(
            WorldProjection::new(160, 120, 320, 64),
            &mut provider,
            trace_target(),
            ThirdPersonCameraInput {
                yaw_delta_q12: 64,
                ..ThirdPersonCameraInput::default()
            },
            ThirdPersonCameraConfig::character(1400, 700, 0),
            1,
        );
        assert_eq!(result, Err(CollisionQueryError));
        assert_eq!(camera, before);
    }

    fn test_ray(
        from: RoomPoint,
        to: RoomPoint,
        distance: i32,
        sector_size: i32,
        vertical_margin: i32,
    ) -> CameraRay {
        CameraRay {
            from,
            to,
            dx: to.x.saturating_sub(from.x),
            dy: to.y.saturating_sub(from.y),
            dz: to.z.saturating_sub(from.z),
            distance,
            sector_size,
            room_width: 1,
            room_depth: 1,
            vertical_margin,
        }
    }

    fn flat_floor_world() -> [u8; 92] {
        floor_world_with_heights([0, 0, 0, 0])
    }

    fn floor_world_with_heights(heights: [i32; 4]) -> [u8; 92] {
        const ASSET_HEADER: usize = 12;
        const WORLD_HEADER: usize = 20;
        const SECTOR_RECORD: usize = 60;
        const SECTOR0: usize = ASSET_HEADER + WORLD_HEADER;
        let payload_len = (WORLD_HEADER + SECTOR_RECORD) as u32;
        let mut buf = [0u8; 92];
        buf[0..4].copy_from_slice(b"PSXW");
        buf[4..6].copy_from_slice(&3u16.to_le_bytes());
        buf[8..12].copy_from_slice(&payload_len.to_le_bytes());
        buf[12..14].copy_from_slice(&1u16.to_le_bytes());
        buf[14..16].copy_from_slice(&1u16.to_le_bytes());
        buf[16..20].copy_from_slice(&1024i32.to_le_bytes());
        buf[20..22].copy_from_slice(&1u16.to_le_bytes());
        buf[22..24].copy_from_slice(&1u16.to_le_bytes());

        buf[SECTOR0] = 1 | 4;
        buf[SECTOR0 + 4..SECTOR0 + 6].copy_from_slice(&0u16.to_le_bytes());
        for (index, height) in heights.iter().enumerate() {
            let start = SECTOR0 + 12 + index * 4;
            buf[start..start + 4].copy_from_slice(&height.to_le_bytes());
        }
        buf
    }

    #[test]
    fn yaw_to_point_matches_cardinal_axes() {
        let origin = RoomPoint::ZERO;
        assert_eq!(yaw_to_point(origin, RoomPoint::new(0, 0, 10)), Angle::ZERO);
        assert_eq!(
            yaw_to_point(origin, RoomPoint::new(10, 0, 0)),
            Angle::QUARTER
        );
        assert_eq!(yaw_to_point(origin, RoomPoint::new(0, 0, -10)), Angle::HALF);
        assert_eq!(
            yaw_to_point(origin, RoomPoint::new(-10, 0, 0)),
            Angle::THREE_QUARTER
        );
    }

    #[test]
    fn approach_angle_takes_shortest_wrapping_path() {
        assert_eq!(
            Angle::from_q12(4090).approach_q12(Angle::from_q12(8), 16),
            Angle::from_q12(8)
        );
        assert_eq!(
            Angle::from_q12(20).approach_q12(Angle::from_q12(4000), 16),
            Angle::from_q12(4)
        );
    }

    #[test]
    fn segment_wall_hit_finds_cardinal_crossing() {
        let from = RoomPoint::new(512, 0, 512);
        let to = RoomPoint::new(1536, 0, 512);
        let heights = [-512, -512, 512, 512];
        let ray = test_ray(from, to, 1024, 1024, 0);
        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_EAST, heights),
            Some(512)
        );
        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_NORTH, heights),
            None
        );
    }

    #[test]
    fn segment_wall_hit_finds_diagonal_crossing() {
        let from = RoomPoint::new(512, 0, 0);
        let to = RoomPoint::new(512, 0, 1024);
        let heights = [-512, -512, 512, 512];
        let ray = test_ray(from, to, 1024, 1024, 0);

        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_NORTH_WEST_SOUTH_EAST, heights),
            Some(512)
        );
        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_NORTH_EAST_SOUTH_WEST, heights),
            Some(512)
        );
    }

    #[test]
    fn segment_wall_hit_ignores_camera_ray_above_wall() {
        let from = RoomPoint::new(512, 900, 512);
        let to = RoomPoint::new(1536, 900, 512);
        let heights = [0, 0, 512, 512];
        let ray = test_ray(from, to, 1024, 1024, 0);

        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_EAST, heights),
            None
        );
    }

    #[test]
    fn movement_does_not_auto_align_by_default() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let config = ThirdPersonCameraConfig::character(1400, 700, 0);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: true,
            lock_target: None,
        };
        camera.snap_to_player_with_yaw(target, config, Angle::HALF.add_signed_q12(128));

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );
        assert_eq!(frame.yaw, Angle::HALF.add_signed_q12(128));
    }

    #[test]
    fn manual_input_sets_cooldown_and_prevents_configured_auto_align() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.auto_align_when_moving = true;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: true,
            lock_target: None,
        };
        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput {
                yaw_delta_q12: 128,
                pitch_delta_q12: 0,
                recenter: false,
            },
            config,
        );
        assert_eq!(frame.yaw, Angle::HALF.add_signed_q12(128));
        assert_eq!(frame.pitch_q12, default_pitch_q12(config));
        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );
        assert_eq!(frame.yaw, Angle::HALF.add_signed_q12(128));
    }

    #[test]
    fn accelerated_orbit_ramps_and_resets_on_release_and_reversal() {
        let normal = accelerated_orbit_step_q12(3, false);
        let fast = accelerated_orbit_step_q12(3, true);
        assert_eq!((normal, fast), (18, 40));
        assert_eq!(
            (
                accelerated_orbit_step_q12(5, false),
                accelerated_orbit_step_q12(5, true)
            ),
            (23, 49)
        );
        let (mut ramp, mut remainder) = (0, 0);
        assert_eq!(
            accelerated_orbit_delta(normal, Some(3), &mut ramp, &mut remainder),
            normal
        );
        for _ in 1..36 {
            let step = accelerated_orbit_delta(normal, Some(3), &mut ramp, &mut remainder);
            assert!((normal..=fast).contains(&step));
        }
        assert_eq!(
            accelerated_orbit_delta(normal, Some(3), &mut ramp, &mut remainder),
            fast
        );
        assert_eq!(
            accelerated_orbit_delta(-normal, Some(3), &mut ramp, &mut remainder),
            -normal
        );
        assert_eq!(
            accelerated_orbit_delta(0, Some(3), &mut ramp, &mut remainder),
            0
        );
        assert_eq!((ramp, remainder), (0, 0));
        assert_eq!(
            accelerated_orbit_delta(-normal, Some(3), &mut ramp, &mut remainder),
            -normal
        );
        assert_eq!(
            accelerated_orbit_delta(normal, None, &mut ramp, &mut remainder),
            normal
        );
        assert_eq!((ramp, remainder), (0, 0));
    }

    #[test]
    fn accelerated_orbit_preserves_small_input_and_signed_symmetry() {
        let (mut positive, mut negative) = (0, 0);
        let (mut positive_rem, mut negative_rem) = (0, 0);
        let (mut sum_positive, mut sum_negative) = (0, 0);
        for _ in 0..360 {
            sum_positive += accelerated_orbit_delta(1, Some(3), &mut positive, &mut positive_rem);
            sum_negative += accelerated_orbit_delta(-1, Some(3), &mut negative, &mut negative_rem);
            assert_eq!(sum_positive, -sum_negative);
        }
        assert!(sum_positive > 700, "fractional fast rates must accumulate");
    }

    #[test]
    fn accelerated_orbit_catchup_matches_individual_ticks_and_stops_on_release() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut config = ThirdPersonCameraConfig::character(219, 113, 73);
        config.accelerated_orbit_speed = Some(3);
        config.recenter_preserves_pitch = true;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        let mut grouped = ThirdPersonCameraState::new(Angle::HALF);
        let mut single = grouped;
        for input in [
            ThirdPersonCameraInput {
                yaw_delta_q12: 18,
                pitch_delta_q12: 3,
                recenter: false,
            },
            ThirdPersonCameraInput {
                yaw_delta_q12: -18,
                pitch_delta_q12: 0,
                recenter: false,
            },
            ThirdPersonCameraInput::default(),
        ] {
            for _ in 0..12 {
                grouped.update_vblanks(projection, None, target, input, config, 4);
                for _ in 0..4 {
                    single.update(projection, None, target, input, config);
                }
                assert_eq!(grouped, single);
            }
        }
        let yaw = single.yaw();
        let pitch = single.pitch_q12;
        single.update(
            projection,
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );
        assert_eq!((single.yaw(), single.pitch_q12), (yaw, pitch));
        grouped.update_vblanks(
            projection,
            None,
            target,
            ThirdPersonCameraInput {
                recenter: true,
                ..Default::default()
            },
            config,
            4,
        );
        for i in 0..4 {
            single.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput {
                    recenter: i == 0,
                    ..Default::default()
                },
                config,
            );
        }
        assert_eq!(grouped, single);
    }

    #[test]
    fn recenter_retains_selected_pitch_and_finishes_in_eighteen_ticks() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        for initial_yaw in [Angle::ZERO, Angle::QUARTER, Angle::from_q12(4090)] {
            for pitch_delta in [-256, 256] {
                let mut config = ThirdPersonCameraConfig::character(219, 113, 73);
                config.recenter_preserves_pitch = true;
                let target = ThirdPersonCameraTarget {
                    player: RoomPoint::ZERO,
                    player_yaw: Angle::ZERO,
                    moving: false,
                    lock_target: None,
                };
                let mut camera = ThirdPersonCameraState::new(initial_yaw);
                camera.snap_to_player_with_yaw(target, config, initial_yaw);
                camera.update(
                    projection,
                    None,
                    target,
                    ThirdPersonCameraInput {
                        pitch_delta_q12: pitch_delta,
                        ..Default::default()
                    },
                    config,
                );
                let retained = camera.pitch_q12;
                for tick in 0..18 {
                    camera.update(
                        projection,
                        None,
                        target,
                        ThirdPersonCameraInput {
                            recenter: tick == 0,
                            ..Default::default()
                        },
                        config,
                    );
                    assert_eq!(camera.pitch_q12, retained);
                }
                assert_eq!(camera.yaw(), Angle::HALF);
                assert!(!camera.recenter_active);
                // The legacy policy remains selectable.
                config.recenter_preserves_pitch = false;
                for tick in 0..18 {
                    camera.update(
                        projection,
                        None,
                        target,
                        ThirdPersonCameraInput {
                            recenter: tick == 0,
                            ..Default::default()
                        },
                        config,
                    );
                }
                assert_eq!(camera.pitch_q12, default_pitch_q12(config));
            }
        }
    }

    #[test]
    fn manual_release_waits_then_gradually_restores_automatic_alignment() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut config = ThirdPersonCameraConfig::character(219, 113, 73);
        config.auto_align_when_moving = true;
        config.manual_cooldown_frames = 120;
        config.manual_release_frames = 60;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: true,
            lock_target: None,
        };
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        camera.update(
            projection,
            None,
            target,
            ThirdPersonCameraInput {
                yaw_delta_q12: 1024,
                ..Default::default()
            },
            config,
        );
        let initial = camera.yaw();
        for _ in 0..120 {
            camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
            assert_eq!(camera.yaw(), initial);
        }
        let mut last_step = 0;
        for tick in 1..=60 {
            let before = camera.yaw();
            camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
            let step = before.shortest_delta_q12(camera.yaw()).unsigned_abs();
            assert_eq!(step, config.auto_align_step.as_q12() * tick / 60);
            assert!(step >= last_step);
            last_step = step;
        }
        assert_eq!(camera.manual_release, 0);
    }

    #[test]
    fn recenter_eases_camera_behind_player_yaw() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let config = ThirdPersonCameraConfig::character(1400, 700, 0);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        camera.snap_to_player_with_yaw(target, config, Angle::QUARTER);

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput {
                yaw_delta_q12: 0,
                pitch_delta_q12: 0,
                recenter: true,
            },
            config,
        );

        assert_eq!(frame.yaw, Angle::QUARTER.add(config.lock_on_align_step));
        // A single press completes the turn after release.
        for _ in 0..32 {
            camera.update(
                WorldProjection::new(160, 120, 320, 64),
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }
        assert_eq!(camera.yaw(), Angle::HALF);
        assert!(!camera.recenter_active);
        camera.recenter_active = true;
        camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput {
                yaw_delta_q12: 20,
                ..ThirdPersonCameraInput::default()
            },
            config,
        );
        assert!(
            !camera.recenter_active,
            "manual orbit must cancel recentering"
        );
    }

    fn profiled_config() -> ThirdPersonCameraConfig {
        let mut config = ThirdPersonCameraConfig::character(219, 113, 73);
        config.lock_height_boost = 11;
        config.fov_y_degrees = 43;
        config.blend_profiles = true;
        config.lock_target_framing = true;
        config.position_vertical_lag_shift = Some(3);
        config.focus_vertical_lag_shift = Some(4);
        config.lock_profile = Some(ThirdPersonCameraProfile {
            distance: 244,
            height: 119,
            target_height: 73,
            fov_y_degrees: 46,
            shoulder_offset: 0,
        });
        config
    }

    #[test]
    fn shoulder_lock_moves_player_left_and_unlock_restores_center() {
        let projection = WorldProjection::new(160, 120, 305, 4);
        let mut config = ThirdPersonCameraConfig::character(208, 144, 80);
        config.blend_profiles = true;
        config.lock_target_framing = true;
        config.fov_y_degrees = 43;
        config.lock_profile = Some(ThirdPersonCameraProfile {
            distance: 160, height: 96, target_height: 80,
            fov_y_degrees: 43, shoulder_offset: 24,
        });
        let mut target = trace_target();
        let mut state = ThirdPersonCameraState::new(Angle::HALF);
        state.snap_to_player(target, config);
        target.lock_target = Some(RoomPoint::new(0, 78, 120));
        let mut previous_x = state.position.x;
        for _ in 0..300 {
            let frame = state.update(projection, None, target, ThirdPersonCameraInput::default(), config);
            assert!((frame.camera.position.x - previous_x).abs() <= 3, "shoulder transition snaps");
            previous_x = frame.camera.position.x;
        }
        let frame = state.current_frame(projection);
        let player = frame.camera.project_world(RoomPoint::new(0, 70, 0)).unwrap();
        let enemy = frame.camera.project_world(target.lock_target.unwrap()).unwrap();
        assert!(player.sx < 130, "player must clear the central view: {player:?}");
        assert!(enemy.sx > player.sx && (enemy.sx - 160).abs() <= 8, "enemy must stay near centre: {enemy:?}");
        assert!(state.position.x <= -22);
        target.lock_target = None;
        for _ in 0..500 {
            state.update(projection, None, target, ThirdPersonCameraInput::default(), config);
        }
        assert_eq!(state.focus.x, 0);
        // Unlock preserves the orbit angle, but removes the shoulder framing.
        let player = state.current_frame(projection).camera
            .project_world(RoomPoint::new(0, 70, 0)).unwrap();
        assert!((player.sx - 160).abs() <= 1, "free camera recentres the player: {player:?}");
        assert_eq!(state.distance(), config.distance);
    }

    struct ShoulderWallTraceProvider;
    impl CollisionTraceProvider for ShoulderWallTraceProvider {
        fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut crate::CollisionTrace) -> bool {
            let mut trace = crate::CollisionTrace::unobstructed(query.end);
            // Wall on the camera's right, in world coordinates for a +Z lock.
            if query.end.x < -12 && query.start.x >= -12 {
                let clear = query.start.x + 12;
                let span = query.start.x - query.end.x;
                trace.fraction_q12 = clear * COLLISION_FRACTION_ONE_Q12 / span;
                trace.end = RoomPoint::new(-12,
                    query.start.y + (query.end.y - query.start.y) * clear / span,
                    query.start.z + (query.end.z - query.start.z) * clear / span);
            }
            *output = trace;
            true
        }
    }

    #[test]
    fn shoulder_pivot_and_eye_stay_inside_side_wall() {
        let mut config = ThirdPersonCameraConfig::character(160, 96, 80);
        config.lock_target_framing = true;
        config.lock_profile = Some(ThirdPersonCameraProfile {
            distance: 160, height: 96, target_height: 80,
            fov_y_degrees: 43, shoulder_offset: 32,
        });
        let target = ThirdPersonCameraTarget {
            lock_target: Some(RoomPoint::new(0, 78, 120)), ..trace_target()
        };
        let mut state = ThirdPersonCameraState::new(Angle::HALF);
        for _ in 0..180 {
            let frame = state.update_vblanks_with_trace_provider(
                WorldProjection::new(160,120,305,4), &mut ShoulderWallTraceProvider,
                target, ThirdPersonCameraInput::default(), config, 1).unwrap();
            assert!(frame.focus.x >= -12, "pivot crossed the wall: {:?}", frame.focus);
            assert!(frame.camera.position.x >= -12, "eye crossed the wall: {:?}", frame.camera.position);
        }
    }

    #[test]
    fn aim_unlock_preserves_pitch_and_free_aim_keeps_collision() {
        let mut config = ThirdPersonCameraConfig::character(208, 144, 80);
        config.blend_profiles = true;
        config.profile_response_q12 = 1024;
        config.preserve_profile_pitch = true;
        config.lock_target_framing = true;
        config.lock_frame_percent = 0;
        config.composition_override = Some(ThirdPersonCameraProfile {
            distance: 160, height: 96, target_height: 80,
            fov_y_degrees: 43, shoulder_offset: 24,
        });
        let projection = WorldProjection::new(160, 120, 305, 4);
        let mut target = ThirdPersonCameraTarget {
            lock_target: Some(RoomPoint::new(0, 78, 180)), ..trace_target()
        };
        let mut state = ThirdPersonCameraState::new(Angle::HALF);
        for _ in 0..120 {
            state.update(projection, None, target, ThirdPersonCameraInput::default(), config);
        }
        let before = state.current_frame(projection);
        let enemy = before.camera.project_world(target.lock_target.unwrap()).unwrap();
        assert!((enemy.sy - 120).abs() <= 3, "aim should centre the shot: {enemy:?}");
        state.release_lock_preserving_view();
        target.lock_target = None;
        for _ in 0..30 {
            let frame = state.update(projection, None, target, ThirdPersonCameraInput::default(), config);
            assert!((frame.pitch_q12 - before.pitch_q12).abs() <= 1);
        }
        let mut state = ThirdPersonCameraState::new(Angle::HALF);
        for _ in 0..120 {
            let frame = state.update_vblanks_with_trace_provider(projection,
                &mut ShoulderWallTraceProvider, target, ThirdPersonCameraInput::default(), config, 1).unwrap();
            assert!(frame.camera.position.x >= -12 && frame.focus.x >= -12);
        }
    }

    #[test]
    fn reference_lock_framing_keeps_player_pivot_and_handles_target_height() {
        let mut config = ThirdPersonCameraConfig::character(158, 61, 61);
        config.lock_target_framing = true;
        config.fov_y_degrees = 43;
        config.pitch_min_q12 = -455;
        config.pitch_max_q12 = 796;
        let projection = WorldProjection::new(160, 120, 320, 4);
        let mut pitches = [0; 3];
        for (index, elevation) in [-50, 61, 206].into_iter().enumerate() {
            let mut target = ThirdPersonCameraTarget {
                player: RoomPoint::ZERO,
                player_yaw: Angle::ZERO,
                moving: false,
                lock_target: Some(RoomPoint::new(0, elevation, 896)),
            };
            let mut camera = ThirdPersonCameraState::new(Angle::HALF);
            for _ in 0..500 {
                camera.update(
                    projection,
                    None,
                    target,
                    ThirdPersonCameraInput::default(),
                    config,
                );
            }
            assert_eq!(camera.focus, RoomPoint::new(0, 61, 0));
            assert_eq!(camera.pitch_q12, 0, "lock leaves the manual pitch intact");
            let frame = camera.current_frame(projection);
            pitches[index] = frame.pitch_q12;
            let enemy = frame
                .camera
                .projection
                .project_view(frame.camera.view_vertex(target.lock_target.unwrap()))
                .expect("enemy remains in front of the camera");
            // 120 - focal(305) * tan(43/2 * .45) is about screen Y=68.
            assert!((64..=72).contains(&enemy.sy), "target framing: {enemy:?}");
            for y in [0, 90] {
                let point = frame
                    .camera
                    .projection
                    .project_view(frame.camera.view_vertex(RoomPoint::new(0, y, 0)))
                    .expect("player stays in front of camera");
                assert!(
                    (0..240).contains(&point.sy),
                    "player height {y}, target height {elevation}: {point:?}"
                );
            }
            target.lock_target = None;
            for _ in 0..200 {
                camera.update(
                    projection,
                    None,
                    target,
                    ThirdPersonCameraInput::default(),
                    config,
                );
            }
            assert_eq!(camera.lock_pitch_offset_q12, 0);
            assert_eq!(camera.current_frame(projection).pitch_q12, 0);
        }
        assert!(pitches[0] > pitches[1] && pitches[1] > pitches[2]);
    }

    #[test]
    fn raised_reference_profile_keeps_lock_anchor_clear_and_restores_free_height() {
        let mut config = ThirdPersonCameraConfig::character(208, 144, 80);
        config.lock_target_framing = true;
        config.fov_y_degrees = 43;
        let projection = WorldProjection::new(160, 120, 305, 4);
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        for _ in 0..200 {
            camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }
        let free = camera.current_frame(projection);
        target.lock_target = Some(RoomPoint::new(0, 78, 384));
        for _ in 0..200 {
            camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }
        let locked = camera.current_frame(projection);
        let screen_y = |point| {
            locked
                .camera
                .projection
                .project_view(locked.camera.view_vertex(point))
                .unwrap()
                .sy
        };
        let anchor_y = screen_y(target.lock_target.unwrap());
        assert!(
            (65..=71).contains(&anchor_y),
            "raised profile must preserve target framing: {anchor_y}"
        );
        let head_y = screen_y(RoomPoint::new(0, 90, 0));
        assert!(
            head_y >= anchor_y + 28,
            "player head must clear the lock anchor: {head_y}/{anchor_y}"
        );
        assert!(
            screen_y(RoomPoint::ZERO) <= 224,
            "player feet retain a bottom margin"
        );
        target.lock_target = None;
        for _ in 0..200 {
            camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }
        let restored = camera.current_frame(projection);
        assert_eq!(restored.camera.position, free.camera.position);
        assert_eq!(restored.pitch_q12, free.pitch_q12);
    }

    #[test]
    fn lock_framing_unlock_restores_the_manual_pitch_with_inherited_lens() {
        let mut config = ThirdPersonCameraConfig::character(158, 61, 61);
        config.lock_target_framing = true;
        let projection = WorldProjection::new(160, 120, 305, 4);
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        camera.update(
            projection,
            None,
            target,
            ThirdPersonCameraInput {
                pitch_delta_q12: 120,
                ..ThirdPersonCameraInput::default()
            },
            config,
        );
        assert_eq!(camera.pitch_q12, 120);
        target.lock_target = Some(RoomPoint::new(0, 61, 400));
        for _ in 0..200 {
            camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }
        assert_eq!(camera.pitch_q12, 120);
        assert!((151..=155).contains(&camera.current_frame(projection).pitch_q12));
        assert_eq!(
            camera.current_frame(projection).camera.projection,
            projection
        );
        target.lock_target = None;
        for _ in 0..200 {
            camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }
        assert_eq!(camera.lock_pitch_offset_q12, 0);
        assert_eq!(camera.current_frame(projection).pitch_q12, 120);
    }

    #[test]
    fn profile_transitions_match_grouped_ticks_and_restore_free_framing() {
        let config = profiled_config();
        let projection = WorldProjection::new(160, 120, 320, 4);
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        let mut one = ThirdPersonCameraState::new(Angle::HALF);
        let mut batch = one;
        for tick in 0..150 {
            target.lock_target = if (10..60).contains(&tick) {
                Some(RoomPoint::new(0, if tick < 30 { 180 } else { 20 }, 400))
            } else {
                None
            };
            for _ in 0..4 {
                one.update(
                    projection,
                    None,
                    target,
                    ThirdPersonCameraInput::default(),
                    config,
                );
            }
            batch.update_vblanks(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
                4,
            );
            assert_eq!(one, batch);
        }
        assert_eq!(one.focus.y, 73);
        assert!((303..=306).contains(&one.current_frame(projection).camera.projection.focal_length));
    }

    #[test]
    fn profile_collision_failure_rolls_back_lens_and_transition_state() {
        let config = profiled_config();
        let projection = WorldProjection::new(160, 120, 320, 4);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut target = trace_target();
        camera.update(
            projection,
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );
        let before = camera;
        target.lock_target = Some(RoomPoint::new(0, 300, 400));
        let mut provider = HalfDistanceTraceProvider {
            fail: true,
            calls: 0,
        };
        assert_eq!(
            camera.update_vblanks_with_trace_provider(
                projection,
                &mut provider,
                target,
                ThirdPersonCameraInput::default(),
                config,
                4
            ),
            Err(CollisionQueryError)
        );
        assert_eq!(camera, before);
    }

    #[test]
    fn character_height_offsets_raise_camera_and_focus() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let config = ThirdPersonCameraConfig::character(1400, 700, 400);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::new(128, 32, -64),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };

        camera.snap_to_player(target, config);

        assert_eq!(camera.focus.y, target.player.y + config.target_height);
        assert_eq!(camera.position.y, target.player.y + config.height);
        assert_eq!(camera.pitch_q12, default_pitch_q12(config));
    }

    #[test]
    fn camera_floor_clearance_lifts_low_camera_position() {
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let mut camera = ThirdPersonCameraState::new(Angle::ZERO);
        let mut config = ThirdPersonCameraConfig::character(384, 0, 0);
        config.min_floor_clearance = 64;
        config.pitch_min_q12 = 0;
        config.pitch_max_q12 = 0;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::new(512, 0, 640),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            Some(room.collision()),
            target,
            ThirdPersonCameraInput::default(),
            config,
        );

        assert_eq!(frame.camera.position.y, 64);
    }

    #[test]
    fn camera_floor_clearance_ignores_saturated_floor_height() {
        let bytes = floor_world_with_heights([i32::MAX; 4]);
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let mut camera = ThirdPersonCameraState::new(Angle::ZERO);
        let mut config = ThirdPersonCameraConfig::character(384, 0, 0);
        config.min_floor_clearance = 64;
        config.pitch_min_q12 = 0;
        config.pitch_max_q12 = 0;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::new(512, 0, 640),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            Some(room.collision()),
            target,
            ThirdPersonCameraInput::default(),
            config,
        );

        assert_eq!(frame.camera.position.y, 0);
    }

    #[test]
    fn camera_collision_stops_at_last_clear_sample_before_void() {
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let mut config = ThirdPersonCameraConfig::character(1536, 0, 0);
        config.min_distance = 0;
        config.collision_margin = 0;
        let from = RoomPoint::new(512, 0, 512);
        let to = RoomPoint::new(512, 0, 2048);

        let clear = probe_clear_distance(room.collision(), from, to, 1536, config);

        assert_eq!(clear, 256);
    }

    #[test]
    fn camera_collision_rooms_cross_active_chunk_boundary() {
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let rooms = [
            CharacterCollisionRoom::new(room, 0, 0),
            CharacterCollisionRoom::new(room, 0, 1024),
        ];
        let mut config = ThirdPersonCameraConfig::character(1280, 0, 0);
        config.min_distance = 0;
        config.collision_margin = 0;
        let from = RoomPoint::new(512, 0, 512);
        let to = RoomPoint::new(512, 0, 1792);

        assert_eq!(
            probe_clear_distance(room.collision(), from, to, 1280, config),
            256
        );
        assert_eq!(
            probe_clear_distance_rooms(&rooms, from, to, 1280, config),
            1280
        );
    }

    #[test]
    fn explicit_start_yaw_does_not_follow_player_yaw() {
        let mut camera = ThirdPersonCameraState::new(Angle::ZERO);
        let config = ThirdPersonCameraConfig::character(1400, 700, 0);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::QUARTER,
            moving: false,
            lock_target: None,
        };

        camera.snap_to_player_with_yaw(target, config, Angle::HALF);

        assert_eq!(camera.yaw(), Angle::HALF);
    }

    #[test]
    fn lock_on_biases_focus_toward_target_without_losing_player_anchor() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 400);
        config.focus_lag_shift = 0;
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::new(128, 32, -64),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        camera.snap_to_player(target, config);

        target.lock_target = Some(RoomPoint::new(4096, 1024, 4096));
        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );

        let player = player_focus(target.player, config.target_height);
        assert_eq!(
            frame.focus,
            camera_focus_goal(target, config, config.distance)
        );
        assert_ne!(frame.focus, player);
        assert!(frame.focus.x > player.x);
        assert_eq!(frame.focus.y, player.y);
        assert!(frame.focus.z > player.z);
    }

    #[test]
    fn shortened_arm_keeps_locked_player_behind_focus() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(2000, 1000, 850);
        config.focus_lag_shift = 0;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: Some(RoomPoint::new(0, 0, 4000)),
        };
        camera.snap_to_player(target, config);
        camera.distance = 240;
        camera.collision_release_delay = 8;
        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );
        assert!(frame.focus.z > target.player.z);
        assert!(frame.focus.z - target.player.z < frame.distance / 2);
        assert_eq!(frame.focus.y, config.target_height);
    }

    #[test]
    fn lock_on_height_converges_to_authored_world_offset() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 400);
        config.focus_lag_shift = 0;
        config.position_lag_shift = 6;
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::new(128, 32, -64),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        camera.snap_to_player(target, config);
        let projection = WorldProjection::new(160, 120, 320, 64);
        let unlocked = camera.current_frame(projection);

        target.lock_target = Some(RoomPoint::new(128, 32, 2048));
        let locked = camera.update(
            projection,
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );

        assert_eq!(locked.focus.y, unlocked.focus.y);
        assert!(locked.camera.position.y > unlocked.camera.position.y);
        assert!(locked.pitch_q12 > unlocked.pitch_q12);
        let expected_locked_y = target
            .player
            .y
            .saturating_add(config.height)
            .saturating_add(config.lock_height_boost);
        assert!(locked.camera.position.y < expected_locked_y);

        let mut converged = locked;
        for _ in 0..256 {
            converged = camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }
        assert_eq!(converged.camera.position.y, expected_locked_y);
    }

    #[test]
    fn unlock_height_converges_back_to_authored_base() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 400);
        config.focus_lag_shift = 0;
        config.position_lag_shift = 6;
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::new(128, 32, -64),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: Some(RoomPoint::new(128, 32, 2048)),
        };
        camera.snap_to_player(target, config);
        for _ in 0..256 {
            camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }

        target.lock_target = None;
        let mut frame = camera.current_frame(projection);
        for _ in 0..256 {
            frame = camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }
        assert_eq!(frame.camera.position.y, target.player.y + config.height);
    }

    #[test]
    fn vertical_follow_softens_height_changes_without_slowing_horizontal_focus() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        for height_delta in [-256, 256] {
            let shared = ThirdPersonCameraConfig::character(2000, 1000, 850);
            let mut split = shared;
            split.position_vertical_lag_shift = Some(3);
            split.focus_vertical_lag_shift = Some(4);
            let mut target = ThirdPersonCameraTarget {
                player: RoomPoint::ZERO,
                player_yaw: Angle::ZERO,
                moving: true,
                lock_target: None,
            };
            let mut ordinary = ThirdPersonCameraState::new(Angle::HALF);
            ordinary.snap_to_player(target, shared);
            let mut softened = ordinary;
            let initial = ordinary.current_frame(projection);
            target.player = RoomPoint::new(256, height_delta, 256);
            let input = ThirdPersonCameraInput::default();
            let a = ordinary.update(projection, None, target, input, shared);
            let b = softened.update(projection, None, target, input, split);
            assert_eq!((b.focus.x, b.focus.z), (a.focus.x, a.focus.z));
            assert!((b.focus.y - initial.focus.y).abs() < (a.focus.y - initial.focus.y).abs());
            assert!(
                (b.camera.position.y - initial.camera.position.y).abs()
                    < (a.camera.position.y - initial.camera.position.y).abs()
            );
            for _ in 0..512 {
                softened.update(projection, None, target, input, split);
            }
            assert_eq!(softened.focus.y, target.player.y + split.target_height);
            assert_eq!(softened.position.y, target.player.y + split.height);
        }
    }

    #[test]
    fn vertical_focus_lag_stays_near_the_player_after_sharp_height_changes() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut config = ThirdPersonCameraConfig::character(219, 113, 73);
        config.position_vertical_lag_shift = Some(3);
        config.focus_vertical_lag_shift = Some(4);
        for delta in [-160, 160] {
            let mut target = trace_target();
            let mut camera = ThirdPersonCameraState::new(Angle::HALF);
            camera.snap_to_player(target, config);
            target.player.y += delta;
            let frame = camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
            let desired_focus_y = target.player.y + config.target_height;
            assert!((frame.focus.y - desired_focus_y).abs() <= config.target_height / 4);
        }
    }

    #[test]
    fn vertical_position_lag_can_change_without_changing_focus() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let shared = ThirdPersonCameraConfig::character(2000, 1000, 850);
        let mut split = shared;
        split.position_vertical_lag_shift = Some(4);
        let target = trace_target();
        let mut ordinary = ThirdPersonCameraState::new(Angle::HALF);
        ordinary.snap_to_player(target, shared);
        let mut softened = ordinary;
        let initial = ordinary.current_frame(projection);
        let input = ThirdPersonCameraInput {
            yaw_delta_q12: 32,
            pitch_delta_q12: 64,
            recenter: false,
        };
        let a = ordinary.update(projection, None, target, input, shared);
        let b = softened.update(projection, None, target, input, split);
        assert_eq!(b.focus, a.focus);
        assert_eq!(b.yaw, a.yaw);
        assert_eq!(
            (b.camera.position.x, b.camera.position.z),
            (a.camera.position.x, a.camera.position.z)
        );
        assert!(
            (b.camera.position.y - initial.camera.position.y).abs()
                < (a.camera.position.y - initial.camera.position.y).abs()
        );
    }

    #[test]
    fn vertical_follow_catchup_matches_display_ticks_and_shared_fallback() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        for steps in [1, 2, 3] {
            for independent in [false, true] {
                let mut config = ThirdPersonCameraConfig::character(2000, 1000, 850);
                config.position_lag_shift = 1;
                config.focus_lag_shift = 3;
                if independent {
                    config.position_vertical_lag_shift = Some(3);
                    config.focus_vertical_lag_shift = Some(4);
                }
                let mut target = trace_target();
                let mut stepped = ThirdPersonCameraState::new(Angle::HALF);
                stepped.snap_to_player(target, config);
                let mut batched = stepped;
                let mut explicit_shared = stepped;
                let mut explicit_config = config;
                if !independent {
                    explicit_config.position_vertical_lag_shift = Some(config.position_lag_shift);
                    explicit_config.focus_vertical_lag_shift = Some(config.focus_lag_shift);
                }
                for i in 0..40 {
                    target.player.y = if i < 20 { 256 } else { -128 };
                    target.player.x += 32;
                    let input = ThirdPersonCameraInput::default();
                    for _ in 0..steps {
                        stepped.update(projection, None, target, input, config);
                    }
                    let frame =
                        batched.update_vblanks(projection, None, target, input, config, steps);
                    let explicit = explicit_shared.update_vblanks(
                        projection,
                        None,
                        target,
                        input,
                        explicit_config,
                        steps,
                    );
                    assert_eq!(frame, stepped.current_frame(projection));
                    assert_eq!(frame, explicit);
                }
            }
        }
    }

    #[test]
    fn vertical_lag_does_not_delay_obstruction_pull_in() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.position_vertical_lag_shift = Some(6);
        config.focus_vertical_lag_shift = Some(6);
        let target = trace_target();
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        camera.snap_to_player(target, config);
        let before = camera.position();
        let frame = camera
            .update_vblanks_with_trace_provider(
                projection,
                &mut CloseDistanceTraceProvider,
                target,
                ThirdPersonCameraInput::default(),
                config,
                1,
            )
            .expect("obstructed camera");
        assert!(frame.collision_pull_in);
        assert!(frame.distance < config.min_distance);
        assert!(frame.camera.position.y < before.y);
    }

    #[test]
    fn player_translation_does_not_collapse_the_camera_arm() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut config = ThirdPersonCameraConfig::character(2000, 1000, 850);
        config.position_lag_shift = 6;
        config.focus_lag_shift = 2;
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: true,
            lock_target: None,
        };
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        camera.snap_to_player(target, config);
        let initial = camera.current_frame(projection);
        let offset = initial.camera.position.z - initial.focus.z;
        for _ in 0..120 {
            target.player.z -= 64;
            let frame = camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
            assert_eq!(frame.camera.position.z - frame.focus.z, offset);
            assert!((frame.focus.z - target.player.z).abs() < 256);
        }
    }

    #[test]
    fn collision_shortening_keeps_lock_height_on_the_arm() {
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1536, 700, 400);
        config.min_distance = 128;
        config.collision_margin = 0;
        config.focus_lag_shift = 0;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::new(512, 0, 512),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: Some(RoomPoint::new(512, 0, 900)),
        };
        camera.snap_to_player(target, config);

        let mut frame = camera.current_frame(projection);
        for _ in 0..256 {
            frame = camera.update(
                projection,
                Some(room.collision()),
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }

        assert!(frame.distance < config.distance);
        // The shortened arm slides the camera toward the focus (spring arm),
        // so both base height and lock lift scale with its distance.
        let focus_y = frame.focus.y;
        let slid_base = focus_y
            + ((target.player.y + config.height - focus_y) as i64 * frame.distance as i64
                / config.distance as i64) as i32;
        assert!(frame.camera.position.y < target.player.y + config.height);
        assert_eq!(
            frame.camera.position.y,
            slid_base + config.lock_height_boost * frame.distance / config.distance
        );
    }

    #[test]
    fn manual_pitch_does_not_suppress_lock_height() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 400);
        config.focus_lag_shift = 0;
        config.position_lag_shift = 0;
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        camera.snap_to_player(target, config);
        let unlocked = camera.update(
            projection,
            None,
            target,
            ThirdPersonCameraInput {
                yaw_delta_q12: 0,
                pitch_delta_q12: 64,
                recenter: false,
            },
            config,
        );

        target.lock_target = Some(RoomPoint::new(0, 0, 2048));
        let mut locked = unlocked;
        for _ in 0..256 {
            locked = camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
        }
        assert_eq!(
            locked.camera.position.y - unlocked.camera.position.y,
            config.lock_height_boost
        );
    }

    #[test]
    fn maximum_lock_rise_keeps_full_player_capsule_in_view() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(3300, 1500, 900);
        config.lock_height_boost = config.height;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: Some(RoomPoint::new(0, 0, 2400)),
        };
        camera.snap_to_player(target, config);

        let mut frame = camera.current_frame(projection);
        let assert_player_framed = |frame: ThirdPersonCameraFrame| {
            let feet = frame
                .camera
                .project_world(target.player)
                .expect("player feet remain in front of camera");
            let head = frame
                .camera
                .project_world(RoomPoint::new(
                    target.player.x,
                    target.player.y + 1024,
                    target.player.z,
                ))
                .expect("player head remains in front of camera");
            assert!((0..240).contains(&feet.sy), "feet clipped at y={}", feet.sy);
            assert!((0..240).contains(&head.sy), "head clipped at y={}", head.sy);
        };
        assert_player_framed(frame);
        for _ in 0..180 {
            frame = camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
            assert_player_framed(frame);
        }

        assert_eq!(frame.focus.y, config.target_height);
    }

    #[test]
    fn close_target_crossing_player_does_not_spin_the_camera() {
        let config = ThirdPersonCameraConfig::character(240, 120, 50);
        let projection = WorldProjection::new(160, 120, 320, 8);
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: Some(RoomPoint::new(0, 0, 200)),
        };
        camera.update(
            projection,
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );
        let yaw = camera.yaw();
        for point in [RoomPoint::new(10, 0, 10), RoomPoint::new(-10, 0, -10)] {
            target.lock_target = Some(point);
            let frame = camera.update(
                projection,
                None,
                target,
                ThirdPersonCameraInput::default(),
                config,
            );
            assert_eq!(frame.yaw, yaw);
        }
    }

    #[test]
    fn lock_on_uses_dedicated_fast_yaw_step() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.auto_align_step = Angle::from_q12(18);
        config.lock_on_align_step = Angle::from_q12(128);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: Some(RoomPoint::new(4096, 0, 0)),
        };

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );

        assert_eq!(frame.yaw, Angle::HALF.add_signed_q12(128));
    }

    #[test]
    fn vblank_delta_matches_repeated_camera_updates() {
        let mut stepped = ThirdPersonCameraState::new(Angle::ZERO);
        let mut caught_up = ThirdPersonCameraState::new(Angle::ZERO);
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.auto_align_step = Angle::from_q12(32);
        config.auto_align_when_moving = true;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::new(1024, 0, 1024),
            player_yaw: Angle::QUARTER,
            moving: true,
            lock_target: None,
        };
        let input = ThirdPersonCameraInput::default();

        stepped.snap_to_player_with_yaw(target, config, Angle::ZERO);
        caught_up.snap_to_player_with_yaw(target, config, Angle::ZERO);
        let _ = stepped.update(projection, None, target, input, config);
        let expected = stepped.update(projection, None, target, input, config);
        let actual = caught_up.update_vblanks(projection, None, target, input, config, 2);

        assert_eq!(actual, expected);
        assert_eq!(caught_up.yaw(), stepped.yaw());
        assert_eq!(caught_up.position(), stepped.position());
        assert_eq!(caught_up.focus(), stepped.focus());
    }

    #[test]
    fn solve_throttle_matches_per_tick_solve_in_static_scene() {
        // With the player parked and no manual input, the sweep inputs
        // are identical every tick once easing converges, so a reused
        // solve equals a fresh one and the throttled camera must track
        // the per-tick camera exactly.
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let rooms = [CharacterCollisionRoom::new(room, 0, 0)];
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut per_tick_config = ThirdPersonCameraConfig::character(1400, 700, 0);
        per_tick_config.min_distance = 0;
        let mut throttled_config = per_tick_config;
        throttled_config.collision_solve_interval = 2;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::new(512, 0, 512),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        let input = ThirdPersonCameraInput::default();
        let mut per_tick = ThirdPersonCameraState::new(Angle::ZERO);
        let mut throttled = ThirdPersonCameraState::new(Angle::ZERO);
        per_tick.snap_to_player(target, per_tick_config);
        throttled.snap_to_player(target, throttled_config);

        for _ in 0..8 {
            let expected = per_tick.update_vblanks_with_collision_rooms(
                projection,
                &rooms,
                target,
                input,
                per_tick_config,
                1,
            );
            let actual = throttled.update_vblanks_with_collision_rooms(
                projection,
                &rooms,
                target,
                input,
                throttled_config,
                1,
            );
            assert_eq!(actual.camera.position, expected.camera.position);
            assert_eq!(actual.distance, expected.distance);
            assert_eq!(actual.focus, expected.focus);
        }
    }

    #[test]
    fn manual_pitch_input_clamps_to_config_limits() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.pitch_min_q12 = -64;
        config.pitch_max_q12 = 96;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput {
                yaw_delta_q12: 0,
                pitch_delta_q12: 512,
                recenter: false,
            },
            config,
        );

        assert_eq!(frame.pitch_q12, 96);
    }
}
