//! Fixed-budget third-person camera controller.
//!
//! The controller is designed for PS1-scale rooms: no heap, no dynamic
//! dispatch, bounded ray work, integer math, and collision probes that
//! read the cooked grid room through [`RoomCollision`]. It supplies the
//! common action-camera pieces a game wants on top of [`WorldCamera`]:
//! manual orbit cooldown, optional automatic re-alignment, camera lag,
//! lock-on facing, and a spring-arm collision solve that shortens the
//! boom without taking yaw control away from the player.

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
    /// Maximum auto-align yaw movement per display frame.
    pub auto_align_step: Angle,
    /// When true, ease the unlocked camera behind player yaw while moving.
    pub auto_align_when_moving: bool,
    /// Maximum lock-on yaw movement per display frame.
    pub lock_on_align_step: Angle,
    /// Position lag strength as a power-of-two divisor.
    pub position_lag_shift: u8,
    /// Focus lag strength as a power-of-two divisor.
    pub focus_lag_shift: u8,
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
            auto_align_step: Angle::from_q12(18),
            auto_align_when_moving: false,
            lock_on_align_step: Angle::from_q12(64),
            position_lag_shift: 2,
            focus_lag_shift: 2,
            distance_lag_shift: 3,
            collision_release_delay_frames: 4,
            collision_solve_interval: 1,
        }
    }
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
    yaw: Angle,
    pitch_q12: i16,
    frame_pitch_q12: i16,
    lock_height_offset: i32,
    base_position_y: i32,
    distance: i32,
    position: RoomPoint,
    focus: RoomPoint,
    manual_cooldown: u8,
    recenter_active: bool,
    collision_release_delay: u8,
    initialized: bool,
    last_pull_in: bool,
    last_rotated: bool,
    solve_phase: u8,
    cached_solve: CollisionSolve,
}

impl ThirdPersonCameraState {
    /// Create a camera state with an initial orbit yaw.
    pub const fn new(yaw: Angle) -> Self {
        Self {
            yaw,
            pitch_q12: 0,
            frame_pitch_q12: 0,
            lock_height_offset: 0,
            base_position_y: 0,
            distance: 0,
            position: RoomPoint::ZERO,
            focus: RoomPoint::ZERO,
            manual_cooldown: 0,
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
        self.yaw = yaw;
        self.recenter_active = false;
        self.distance = config
            .distance
            .clamp(config.min_distance, config.max_distance);
        self.pitch_q12 = default_pitch_q12(config);
        self.frame_pitch_q12 = self.pitch_q12;
        self.lock_height_offset = 0;
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
        self.collision_release_delay = 0;
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
        let steps = delta_vblanks.clamp(1, MAX_CAMERA_CATCHUP_VBLANKS);
        let config = normalize_config(config);
        let mut i = 0;
        while i < steps {
            self.advance_one_vblank(collision, target, input, config)?;
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

        let focus_goal = camera_focus_goal(target, config);

        if input.recenter { self.recenter_active = true; }
        if target.lock_target.is_some() { self.recenter_active = false; }

        if input.yaw_delta_q12 != 0 || input.pitch_delta_q12 != 0 {
            self.recenter_active = false;
            self.yaw = self.yaw.add_signed_q12(input.yaw_delta_q12);
            self.pitch_q12 = self
                .pitch_q12
                .saturating_add(input.pitch_delta_q12)
                .clamp(config.pitch_min_q12, config.pitch_max_q12);
            self.manual_cooldown = config.manual_cooldown_frames;
        } else if self.manual_cooldown != 0 {
            self.manual_cooldown -= 1;
        }

        let player_back_yaw = target.player_yaw.add(Angle::HALF);
        let (desired_yaw, yaw_step) = if let Some(lock) = target.lock_target {
            let dx = lock.x.saturating_sub(target.player.x).saturating_abs();
            let dz = lock.z.saturating_sub(target.player.z).saturating_abs();
            let close_radius = (config.distance / 6).max(8);
            // At body contact the target bearing becomes unstable and flips
            // by 180 degrees when it crosses the player. Keep the orbit steady
            // in that small zone; focus still follows both actors.
            let bearing = if dx.max(dz) < close_radius { self.yaw }
                else { yaw_to_point(target.player, lock).add(Angle::HALF) };
            (bearing, config.lock_on_align_step)
        } else if self.recenter_active
            || (config.auto_align_when_moving && target.moving && self.manual_cooldown == 0)
        {
            (player_back_yaw, if self.recenter_active { config.lock_on_align_step } else { config.auto_align_step })
        } else {
            (self.yaw, config.auto_align_step)
        };
        self.yaw = self.yaw.approach_q12(desired_yaw, yaw_step.as_q12());
        if self.recenter_active {
            self.pitch_q12 = approach_i16(
                self.pitch_q12,
                default_pitch_q12(config),
                config.lock_on_align_step.as_q12() as i16,
            );
            if self.yaw == player_back_yaw && self.pitch_q12 == default_pitch_q12(config) {
                self.recenter_active = false;
            }
        }

        self.focus = approach_vertex_shift(self.focus, focus_goal, config.focus_lag_shift);

        // Lock-on height is an authored world-space lift, not an orbit-pitch
        // hint. Ease that lift once, independently of position lag and the
        // collision-shortened spring arm, so it converges to the exact
        // requested camera height without changing the player focus.
        let lock_height_goal = if target.lock_target.is_some() {
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
        let base_camera_y_goal = camera_height_goal(target.player, self.pitch_q12, config);
        let locked_camera_y_goal = base_camera_y_goal.saturating_add(self.lock_height_offset);

        // Spring-arm sweep throttle: the sweep dominates the camera's
        // per-tick cost, and between solves the focus/yaw move by one
        // tick of easing, so reusing the previous solve only delays
        // collision reaction by up to (interval - 1) ticks. Deliberate
        // camera moves (manual orbit, lock-on, recenter) always solve
        // fresh so the throttle never fights the player's hand.
        let solve_now = self.solve_phase == 0
            || input.yaw_delta_q12 != 0
            || input.pitch_delta_q12 != 0
            || input.recenter || self.recenter_active
            || target.lock_target.is_some();
        self.solve_phase = self.solve_phase.saturating_add(1);
        if self.solve_phase >= config.collision_solve_interval.max(1) {
            self.solve_phase = 0;
        }
        let collision_solve = if solve_now {
            let solve = collision.solve(
                self.focus,
                self.yaw,
                self.pitch_q12,
                locked_camera_y_goal,
                config,
            )?;
            self.cached_solve = solve;
            solve
        } else {
            self.cached_solve
        };

        if collision_solve.distance < self.distance {
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
        // looking straight down. The lock-on boost stays additive on top.
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
            self.pitch_q12,
            base_camera_y_goal,
        );
        if collision_solve.pull_in {
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
                config.position_lag_shift,
            );
        }
        self.position.y = self.base_position_y.saturating_add(self.lock_height_offset);
        self.position = collision.clamp_to_floor(self.position, config.min_floor_clearance)?;
        self.frame_pitch_q12 = self
            .pitch_q12
            .saturating_add(lock_pitch_offset_q12(config, self.lock_height_offset));

        self.last_pull_in = collision_solve.pull_in;
        self.last_rotated = false;
        Ok(())
    }

    fn current_frame(&self, projection: WorldProjection) -> ThirdPersonCameraFrame {
        ThirdPersonCameraFrame {
            camera: camera_from_position_focus(projection, self.position, self.focus),
            focus: self.focus,
            yaw: self.yaw,
            pitch_q12: self.frame_pitch_q12,
            distance: self.distance,
            collision_pull_in: self.last_pull_in,
            collision_rotated: self.last_rotated,
        }
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
    config.focus_lag_shift = config.focus_lag_shift.min(6);
    config.distance_lag_shift = config.distance_lag_shift.min(6);
    config.lock_height_boost = config.lock_height_boost.max(0);
    config.collision_solve_interval = config.collision_solve_interval.clamp(1, 4);
    config
}

fn player_focus(player: RoomPoint, target_height: i32) -> RoomPoint {
    RoomPoint::new(player.x, player.y.saturating_add(target_height), player.z)
}

fn camera_focus_goal(
    target: ThirdPersonCameraTarget,
    config: ThirdPersonCameraConfig,
) -> RoomPoint {
    let player = player_focus(target.player, config.target_height);
    let Some(lock) = target.lock_target else {
        return player;
    };

    // Bias a quarter of the way toward the target so both combatants remain
    // legible. Keep the vertical focus on the player: lock-on elevation is a
    // camera-orbit adjustment, and must never drag the player down and out of
    // frame. Cap the horizontal offset relative to spring-arm length so a
    // distant target cannot drag the player out of frame during break grace.
    let target_focus = player_focus(lock, config.target_height);
    let blended = lerp_vertex(player, target_focus, 1, 4);
    let max_offset = (config.distance / 3).max(1);
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

    nearest
        .saturating_sub(config.collision_margin)
        .clamp(0, config.distance)
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

    nearest
        .saturating_sub(config.collision_margin)
        .clamp(0, config.distance)
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
    WorldCamera {
        position,
        projection,
        sin_yaw: Q12::from_ratio(dx, radius),
        cos_yaw: Q12::from_ratio(dz, radius),
        sin_pitch: Q12::from_ratio(target_dy, pitch_len),
        cos_pitch: Q12::from_ratio(radius, pitch_len),
    }
}

fn default_pitch_q12(config: ThirdPersonCameraConfig) -> i16 {
    pitch_from_vertical_distance(
        config.height.saturating_sub(config.target_height),
        config.distance,
    )
    .clamp(config.pitch_min_q12, config.pitch_max_q12)
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

fn approach_vertex_shift(current: RoomPoint, target: RoomPoint, shift: u8) -> RoomPoint {
    RoomPoint::new(
        approach_i32_shift(current.x, target.x, shift),
        approach_i32_shift(current.y, target.y, shift),
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

    fn trace_target() -> ThirdPersonCameraTarget {
        ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        }
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
            camera.update(WorldProjection::new(160, 120, 320, 64), None,
                target, ThirdPersonCameraInput::default(), config);
        }
        assert_eq!(camera.yaw(), Angle::HALF);
        assert!(!camera.recenter_active);
        camera.recenter_active = true;
        camera.update(WorldProjection::new(160, 120, 320, 64), None,
            target, ThirdPersonCameraInput { yaw_delta_q12: 20, ..ThirdPersonCameraInput::default() }, config);
        assert!(!camera.recenter_active, "manual orbit must cancel recentering");
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
        assert_eq!(frame.focus, camera_focus_goal(target, config));
        assert_ne!(frame.focus, player);
        assert!(frame.focus.x > player.x);
        assert_eq!(frame.focus.y, player.y);
        assert!(frame.focus.z > player.z);
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
    fn collision_shortening_does_not_reduce_lock_height() {
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
        // so the base height scales with the distance; the lock boost stays
        // additive on top of that.
        let focus_y = frame.focus.y;
        let slid_base = focus_y
            + ((target.player.y + config.height - focus_y) as i64 * frame.distance as i64
                / config.distance as i64) as i32;
        assert!(frame.camera.position.y < target.player.y + config.height);
        assert_eq!(
            frame.camera.position.y,
            slid_base + config.lock_height_boost
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
            player: RoomPoint::ZERO, player_yaw: Angle::ZERO, moving: false,
            lock_target: Some(RoomPoint::new(0, 0, 200)),
        };
        camera.update(projection, None, target, ThirdPersonCameraInput::default(), config);
        let yaw = camera.yaw();
        for point in [RoomPoint::new(10, 0, 10), RoomPoint::new(-10, 0, -10)] {
            target.lock_target = Some(point);
            let frame = camera.update(projection, None, target, ThirdPersonCameraInput::default(), config);
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
