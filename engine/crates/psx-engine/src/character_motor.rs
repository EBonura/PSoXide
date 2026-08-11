//! Fixed-budget third-person character motor.
//!
//! The motor owns player locomotion state that should be shared by
//! game code and AI controllers: position, yaw, stamina, short evade
//! actions, and the coarse collision commit against cooked room data.
//! Inputs are intent-shaped rather than pad-shaped so callers can feed
//! either player controls or future behaviour-tree output.

use crate::floor_sample::{height_at_local, triangle_heights_to_quad};
use crate::{
    collision_query::{
        trace_collision, CollisionQueryError, CollisionTrace, CollisionTraceProvider,
        CollisionTraceQuery, CollisionTraceShape, COLLISION_FRACTION_ONE_Q12,
    },
    fixed::div_q12_i32,
    Angle, RoomCollision, RoomPoint, RuntimeCollisionRoom, RuntimeRoom, Q12,
};
use psx_math::int32::{abs_i32, isqrt_i32, square_i32_saturating};

const DEFAULT_STAMINA_MAX_Q12: i32 = 4096;
const DEFAULT_BODY_HEIGHT: i32 = 768;
/// Max height (engine units) of a wall the character steps over instead
/// of being blocked by. A riser whose top is within this of the feet is
/// treated as a step (the floor probe already found walkable floor on the
/// far side, so the body simply rises onto it); taller walls still block.
/// Sits in the gap between demo-scale steps (<=~576) and real walls
/// (>=~1152).
const STEP_UP_HEIGHT: i32 = 640;
/// Largest drop the feet snap straight down to (the descent counterpart of
/// [`STEP_UP_HEIGHT`]): walking down demo-scale steps stays glued to the
/// floor. A larger drop -- a ledge, or a hole over a lower floor -- leaves
/// the body airborne instead, and [`CharacterMotorState::apply_vertical`]
/// lets it fall.
const STEP_DOWN_HEIGHT: i32 = 640;
/// Downward acceleration applied to an airborne body each fixed tick, in
/// engine units per tick^2. Integer fixed-point (the PS1 has no FPU).
const GRAVITY_PER_TICK: i32 = 96;
/// Q8 gravity multiplier representing 1.0x body weight.
const DEFAULT_WEIGHT_Q8: u16 = 256;
const MIN_WEIGHT_Q8: u16 = 1;
const MAX_WEIGHT_Q8: u16 = 4096;
/// Terminal downward speed in engine units per tick, so a long fall stays
/// bounded and deterministic.
const MAX_FALL_SPEED: i32 = 768;
const MAX_MOTOR_CATCHUP_VBLANKS: u16 = 4;
/// Maximum downward BSP/provider probe in engine world units.
const TRACE_FLOOR_PROBE_DOWN: i32 = 32_767;
/// Lift a grounded trace origin clear of the supporting plane's epsilon band.
const TRACE_FLOOR_PROBE_LIFT: i32 = 1;
const DIR_NORTH: u8 = 0;
const DIR_EAST: u8 = 1;
const DIR_SOUTH: u8 = 2;
const DIR_WEST: u8 = 3;
const DIR_NORTH_WEST_SOUTH_EAST: u8 = 4;
const DIR_NORTH_EAST_SOUTH_WEST: u8 = 5;

/// Vertical cylinder used by coarse character collision.
///
/// `position` is the floor anchor / bottom centre. The occupied
/// volume spans `radius` in X/Z and `height` upward from `position.y`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CharacterCollisionCylinder {
    /// Bottom-centre room-local position.
    pub position: RoomPoint,
    /// Horizontal radius in engine units.
    pub radius: i32,
    /// Vertical height in engine units.
    pub height: i32,
}

impl CharacterCollisionCylinder {
    /// Empty non-blocking cylinder for fixed stack buffers.
    pub const EMPTY: Self = Self {
        position: RoomPoint::ZERO,
        radius: 0,
        height: 0,
    };

    /// Build a blocking cylinder from a floor anchor, radius, and height.
    pub const fn new(position: RoomPoint, radius: i32, height: i32) -> Self {
        Self {
            position,
            radius,
            height,
        }
    }
}

/// Axis-aligned box used by static prop collision.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CharacterCollisionAabb {
    /// Minimum room-local corner.
    pub min: RoomPoint,
    /// Maximum room-local corner.
    pub max: RoomPoint,
}

impl CharacterCollisionAabb {
    /// Empty non-blocking box for fixed stack buffers.
    pub const EMPTY: Self = Self {
        min: RoomPoint::ZERO,
        max: RoomPoint::ZERO,
    };

    /// Build a blocking AABB from room-local corners.
    pub const fn new(min: RoomPoint, max: RoomPoint) -> Self {
        Self { min, max }
    }

    /// Whether this record describes one finite-volume box in canonical
    /// minimum/maximum order.
    pub const fn is_strictly_valid(self) -> bool {
        self.min.x < self.max.x && self.min.y < self.max.y && self.min.z < self.max.z
    }
}

/// Deterministic actor-cylinder layer over one world trace provider.
///
/// The wrapped provider remains authoritative for static world geometry and
/// transformed brush models. Dynamic blockers are evaluated afterward in
/// slice order, and only a strictly earlier actor contact replaces the world
/// result. Exact ties therefore remain stable: world before actors, then the
/// first actor in the caller-owned slice. The adapter owns no heap or scratch;
/// a wrapped-provider failure leaves the caller's output untouched.
pub struct CharacterBlockerTraceProvider<'provider, 'blockers, P: ?Sized> {
    provider: &'provider mut P,
    blockers: &'blockers [CharacterCollisionCylinder],
    aabb_blockers: &'blockers [CharacterCollisionAabb],
}

impl<'provider, 'blockers, P: CollisionTraceProvider + ?Sized>
    CharacterBlockerTraceProvider<'provider, 'blockers, P>
{
    /// Compose dynamic actor cylinders over `provider` without allocation.
    pub const fn new(
        provider: &'provider mut P,
        blockers: &'blockers [CharacterCollisionCylinder],
    ) -> Self {
        Self {
            provider,
            blockers,
            aabb_blockers: &[],
        }
    }

    /// Compose dynamic actor cylinders and static prop AABBs over `provider`
    /// without allocation. World geometry wins exact trace-fraction ties,
    /// followed by cylinders and then AABBs in caller-owned slice order.
    pub const fn new_with_aabbs(
        provider: &'provider mut P,
        blockers: &'blockers [CharacterCollisionCylinder],
        aabb_blockers: &'blockers [CharacterCollisionAabb],
    ) -> Self {
        Self {
            provider,
            blockers,
            aabb_blockers,
        }
    }
}

impl<P: CollisionTraceProvider + ?Sized> CollisionTraceProvider
    for CharacterBlockerTraceProvider<'_, '_, P>
{
    fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool {
        if self.aabb_blockers.len() > psx_level::MAX_STATIC_PROP_AABB_BLOCKERS
            || self
                .aabb_blockers
                .iter()
                .any(|blocker| !blocker.is_strictly_valid())
        {
            return false;
        }
        let mut best = CollisionTrace::unobstructed(query.end);
        if !self.provider.trace_into(query, &mut best) {
            return false;
        }
        for &blocker in self.blockers {
            if let Some(candidate) = trace_character_blocker(query, blocker) {
                merge_collision_trace(&mut best, candidate);
            }
        }
        for &blocker in self.aabb_blockers {
            if let Some(candidate) = trace_aabb_blocker(query, blocker) {
                merge_collision_trace(&mut best, candidate);
            }
        }
        *output = best;
        true
    }
}

/// One room collision view placed in the motor's current local
/// coordinate space.
///
/// Chunked levels keep the player expressed in the current chunk's
/// room-local coordinates. Adjacent chunks are therefore queried by
/// subtracting their offset from that same current-space point.
#[derive(Copy, Clone, Debug)]
pub struct CharacterCollisionRoom<'room> {
    /// Runtime room/chunk handle.
    pub room: Option<RuntimeCollisionRoom<'room>>,
    /// Offset from the motor's current room origin to this room's
    /// origin, in engine units.
    pub offset_x: i32,
    /// Offset from the motor's current room origin to this room's
    /// origin, in engine units.
    pub offset_z: i32,
    /// Vertical offset from the motor's current room elevation to this
    /// room's, in engine units. Stacked floors are separate collision
    /// rooms at distinct `origin_y`; this lets the motor see an upper
    /// floor's surface at its true height (so you can step up onto it)
    /// instead of collapsed to the current room's elevation.
    pub offset_y: i32,
}

impl<'room> CharacterCollisionRoom<'room> {
    /// Empty non-colliding placeholder for fixed stack buffers.
    pub const EMPTY: Self = Self {
        room: None,
        offset_x: 0,
        offset_z: 0,
        offset_y: 0,
    };

    /// Build a collision room with a current-space origin offset.
    pub const fn new(room: RuntimeRoom<'room>, offset_x: i32, offset_z: i32) -> Self {
        Self {
            room: Some(RuntimeCollisionRoom::Runtime(room)),
            offset_x,
            offset_z,
            offset_y: 0,
        }
    }

    /// Build a collision room from an explicit collision payload source.
    pub const fn from_collision(
        room: RuntimeCollisionRoom<'room>,
        offset_x: i32,
        offset_z: i32,
    ) -> Self {
        Self {
            room: Some(room),
            offset_x,
            offset_z,
            offset_y: 0,
        }
    }

    /// Set the vertical (elevation) offset, for stacked-floor collision
    /// rooms. Builder form keeps the common offset_y=0 constructors terse.
    pub const fn with_offset_y(mut self, offset_y: i32) -> Self {
        self.offset_y = offset_y;
        self
    }
}

/// Collision inputs consumed by [`CharacterMotorState`].
#[derive(Copy, Clone, Debug)]
pub struct CharacterCollision<'room, 'room_ref, 'blockers> {
    /// Optional room grid collision.
    pub room: Option<RoomCollision<'room, 'room_ref>>,
    /// Optional multi-room collision set, in the same current-space
    /// coordinate system as the motor. When present, this takes
    /// precedence over `room`.
    pub rooms: &'blockers [CharacterCollisionRoom<'room>],
    /// Other coarse actor bodies that block this motor.
    pub blockers: &'blockers [CharacterCollisionCylinder],
    /// Static axis-aligned prop bodies that block this motor.
    pub aabb_blockers: &'blockers [CharacterCollisionAabb],
}

impl<'room, 'room_ref, 'blockers> CharacterCollision<'room, 'room_ref, 'blockers> {
    /// Build a collision context from an optional room and blocker slice.
    pub const fn new(
        room: Option<RoomCollision<'room, 'room_ref>>,
        blockers: &'blockers [CharacterCollisionCylinder],
    ) -> Self {
        Self {
            room,
            rooms: &[],
            blockers,
            aabb_blockers: &[],
        }
    }

    /// Build a collision context from an optional room, actor
    /// cylinders, and static AABB blockers.
    pub const fn new_with_aabbs(
        room: Option<RoomCollision<'room, 'room_ref>>,
        blockers: &'blockers [CharacterCollisionCylinder],
        aabb_blockers: &'blockers [CharacterCollisionAabb],
    ) -> Self {
        Self {
            room,
            rooms: &[],
            blockers,
            aabb_blockers,
        }
    }

    /// Build a collision context from multiple offset room chunks.
    pub const fn rooms(
        rooms: &'blockers [CharacterCollisionRoom<'room>],
        blockers: &'blockers [CharacterCollisionCylinder],
    ) -> Self {
        Self {
            room: None,
            rooms,
            blockers,
            aabb_blockers: &[],
        }
    }

    /// Build a multi-room collision context with static AABB blockers.
    pub const fn rooms_with_aabbs(
        rooms: &'blockers [CharacterCollisionRoom<'room>],
        blockers: &'blockers [CharacterCollisionCylinder],
        aabb_blockers: &'blockers [CharacterCollisionAabb],
    ) -> Self {
        Self {
            room: None,
            rooms,
            blockers,
            aabb_blockers,
        }
    }

    /// Build a context that only checks room geometry.
    pub const fn room(room: Option<RoomCollision<'room, 'room_ref>>) -> Self {
        Self {
            room,
            rooms: &[],
            blockers: &[],
            aabb_blockers: &[],
        }
    }
}

trait CharacterCollisionBackend {
    fn supporting_floor(
        &mut self,
        position: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<i32>, CollisionQueryError>;

    fn stand_position(
        &mut self,
        start: RoomPoint,
        target: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<RoomPoint>, CollisionQueryError>;

    fn recovery_position(
        &mut self,
        start: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<RoomPoint>, CollisionQueryError>;

    fn has_world_collision(&self) -> bool;
}

struct GridCharacterCollision<'room, 'room_ref, 'blockers> {
    collision: CharacterCollision<'room, 'room_ref, 'blockers>,
}

impl CharacterCollisionBackend for GridCharacterCollision<'_, '_, '_> {
    fn supporting_floor(
        &mut self,
        position: RoomPoint,
        _shape: CollisionTraceShape,
    ) -> Result<Option<i32>, CollisionQueryError> {
        Ok(supporting_floor_height(
            &self.collision,
            position.x,
            position.z,
            position.y,
        ))
    }

    fn stand_position(
        &mut self,
        _start: RoomPoint,
        target: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<RoomPoint>, CollisionQueryError> {
        let CollisionTraceShape::Body { radius, height } = shape else {
            return Err(CollisionQueryError);
        };
        Ok(body_stand_position(self.collision, target, radius, height))
    }

    fn recovery_position(
        &mut self,
        start: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<RoomPoint>, CollisionQueryError> {
        let CollisionTraceShape::Body { height, .. } = shape else {
            return Err(CollisionQueryError);
        };
        Ok(body_stand_position(self.collision, start, 0, height))
    }

    fn has_world_collision(&self) -> bool {
        self.collision.room.is_some()
            || !self.collision.rooms.is_empty()
            || !self.collision.blockers.is_empty()
            || !self.collision.aabb_blockers.is_empty()
    }
}

struct TraceCharacterCollision<'provider, P: ?Sized> {
    provider: &'provider mut P,
}

impl<P: CollisionTraceProvider + ?Sized> CharacterCollisionBackend
    for TraceCharacterCollision<'_, P>
{
    fn supporting_floor(
        &mut self,
        position: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<i32>, CollisionQueryError> {
        trace_supporting_floor(self.provider, position, shape)
    }

    fn stand_position(
        &mut self,
        start: RoomPoint,
        target: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<RoomPoint>, CollisionQueryError> {
        trace_stand_position(self.provider, start, target, shape)
    }

    fn recovery_position(
        &mut self,
        start: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<RoomPoint>, CollisionQueryError> {
        trace_stand_position(self.provider, start, start, shape)
    }

    fn has_world_collision(&self) -> bool {
        true
    }
}

/// Tunables for [`CharacterMotorState`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CharacterMotorConfig {
    /// Vertical-cylinder radius in world units.
    pub radius: i32,
    /// Vertical-cylinder height in world units.
    pub height: i32,
    /// Forward/backward walking speed in world units per display frame.
    pub walk_speed: i32,
    /// Sprint speed in world units per display frame.
    pub run_speed: i32,
    /// Turn speed per display frame.
    pub yaw_step: Angle,
    /// Downward acceleration in engine units per fixed 60 Hz tick squared.
    pub gravity_per_tick: i32,
    /// Gravity multiplier in Q8 fixed point (`256 = 1.0x`).
    pub weight_q8: u16,
    /// Maximum stamina, in Q12-style arbitrary units.
    pub stamina_max_q12: i32,
    /// Minimum stamina required to start sprinting.
    pub sprint_min_q12: i32,
    /// Stamina spent per sprinting display frame.
    pub sprint_drain_q12: i32,
    /// Stamina recovered per grounded non-sprint display frame.
    pub stamina_recover_q12: i32,
    /// Stamina spent to start a roll.
    pub roll_cost_q12: i32,
    /// Roll travel speed in world units per display frame.
    pub roll_speed: i32,
    /// Display frames where roll keeps moving.
    pub roll_active_frames: u8,
    /// Recovery display frames after roll movement ends.
    pub roll_recovery_frames: u8,
    /// Roll invulnerability display frames from action start.
    pub roll_invulnerable_frames: u8,
    /// Legacy quickstep stamina cost retained for downstream compatibility.
    ///
    /// Kept under the legacy `backstep_*` field names so existing cooked
    /// character records remain binary-compatible.
    pub backstep_cost_q12: i32,
    /// Legacy quickstep travel speed in world units per display frame.
    pub backstep_speed: i32,
    /// Legacy quickstep active movement frames.
    pub backstep_active_frames: u8,
    /// Legacy quickstep recovery frames.
    pub backstep_recovery_frames: u8,
    /// Legacy quickstep invulnerability frames from action start.
    pub backstep_invulnerable_frames: u8,
}

impl CharacterMotorConfig {
    /// Build a motor config from authored Character movement fields.
    pub const fn character(radius: i32, walk_speed: i32, run_speed: i32, yaw_step: Angle) -> Self {
        Self::character_with_body(radius, DEFAULT_BODY_HEIGHT, walk_speed, run_speed, yaw_step)
    }

    /// Build a motor config with explicit coarse collision body dimensions.
    pub const fn character_with_body(
        radius: i32,
        height: i32,
        walk_speed: i32,
        run_speed: i32,
        yaw_step: Angle,
    ) -> Self {
        Self {
            radius,
            height,
            walk_speed,
            run_speed,
            yaw_step,
            gravity_per_tick: GRAVITY_PER_TICK,
            weight_q8: DEFAULT_WEIGHT_Q8,
            stamina_max_q12: DEFAULT_STAMINA_MAX_Q12,
            sprint_min_q12: 384,
            sprint_drain_q12: 40,
            stamina_recover_q12: 36,
            roll_cost_q12: 768,
            roll_speed: 96,
            roll_active_frames: 14,
            roll_recovery_frames: 12,
            roll_invulnerable_frames: 10,
            backstep_cost_q12: 512,
            backstep_speed: 72,
            backstep_active_frames: 8,
            backstep_recovery_frames: 10,
            backstep_invulnerable_frames: 6,
        }
    }
}

/// Per-display-frame abstract movement intent.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CharacterMotorInput {
    /// Signed turn intent. Negative turns left, positive turns right.
    pub turn: i8,
    /// Signed forward/back intent. Negative backs up, positive walks forward.
    pub walk: i8,
    /// World-space analog X movement intent. [`Q12::ONE`] is
    /// full-strength movement to +X. When either analog movement
    /// component is non-zero, the motor uses this vector instead of
    /// tank-style `turn` / `walk`.
    pub move_x: Q12,
    /// World-space analog Z movement intent. [`Q12::ONE`] is
    /// full-strength movement to +Z.
    pub move_z: Q12,
    /// Optional world-space yaw the actor should keep facing while
    /// moving. Lock-on controllers set this to the target direction;
    /// free movement leaves it unset and faces the movement vector.
    pub facing_yaw: Option<Angle>,
    /// True while the actor wants to spend stamina on sprinting.
    pub sprint: bool,
    /// Rising-edge evade request. Directional input produces a roll in both
    /// free movement and lock-on; lock-on remains active through the action.
    pub evade: bool,
}

/// Current high-level action.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CharacterMotorAction {
    /// No fixed action is currently playing.
    Idle,
    /// Directional evasive roll.
    Roll,
    /// Legacy quickstep action retained for downstream compatibility.
    Quickstep,
}

impl CharacterMotorAction {
    /// `true` when no fixed action is currently playing.
    pub const fn is_idle(self) -> bool {
        matches!(self, Self::Idle)
    }
}

/// Animation intent produced by the motor.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CharacterMotorAnim {
    /// Standing still.
    Idle,
    /// Walking or backing up.
    Walk,
    /// Locked-on backward locomotion while still facing the target.
    WalkBackward,
    /// Locked-on left strafe while still facing the target.
    StrafeLeft,
    /// Locked-on right strafe while still facing the target.
    StrafeRight,
    /// Sprinting.
    Run,
    /// Directional evasive roll.
    Roll,
    /// Legacy quickstep animation intent retained for compatibility.
    Quickstep,
    /// Locked-on left evade slide while preserving facing.
    DashLeft,
    /// Locked-on right evade slide while preserving facing.
    DashRight,
}

/// Result of one motor update.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CharacterMotorFrame {
    /// Current root position.
    pub position: RoomPoint,
    /// Current facing yaw.
    pub yaw: Angle,
    /// Animation intent for this frame.
    pub anim: CharacterMotorAnim,
    /// Current fixed action, if any.
    pub action: CharacterMotorAction,
    /// True when the root position changed this frame.
    pub moved: bool,
    /// True when requested movement hit coarse room collision.
    pub blocked: bool,
    /// True while a successful sprint is active.
    pub sprinting: bool,
    /// True during action invulnerability frames.
    pub invulnerable: bool,
    /// True during the non-moving tail of a fixed action.
    pub recovery: bool,
    /// Current stamina after this frame.
    pub stamina_q12: i32,
}

/// Runtime character motor state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CharacterMotorState {
    position: RoomPoint,
    yaw: Angle,
    stamina_q12: i32,
    action: CharacterMotorAction,
    action_frame: u8,
    action_yaw: Angle,
    /// Animation intent chosen when the current action started. Lock-on
    /// evades slide sideways or backward while facing the target, so the
    /// clip choice cannot be derived from the action alone.
    action_anim: CharacterMotorAnim,
    /// Sprint is latched while the button stays held so
    /// `sprint_min_q12` means "minimum to start", not "minimum to
    /// continue".
    sprint_latched: bool,
    /// Prevents held-sprint from pulsing Run/Walk every recovery
    /// frame after stamina reaches zero.
    sprint_exhausted: bool,
    /// Vertical velocity in engine units per tick (negative = falling).
    /// Non-zero only while the body is airborne over a ledge or hole;
    /// reset to zero on landing.
    velocity_y: i32,
    /// `true` while the feet rest on a floor. Gates the per-tick vertical
    /// work: a grounded body that has not moved in XZ reuses its cached
    /// floor and skips the multi-room ground query entirely. Cleared on
    /// teleport and whenever the body goes airborne.
    grounded: bool,
    /// Cached supporting floor height (current space) for the grounded
    /// fast path; only meaningful while `grounded` is set.
    ground_floor: i32,
    /// XZ at which `ground_floor` was last resolved. While the body stays
    /// on this exact cell the floor cannot change (static world), so the
    /// ground query is skipped. Any XZ movement re-resolves.
    ground_anchor_x: i32,
    ground_anchor_z: i32,
}

impl CharacterMotorState {
    /// Create a motor at a root position and yaw.
    pub const fn new(position: RoomPoint, yaw: Angle) -> Self {
        Self {
            position,
            yaw,
            stamina_q12: DEFAULT_STAMINA_MAX_Q12,
            action: CharacterMotorAction::Idle,
            action_frame: 0,
            action_yaw: yaw,
            action_anim: CharacterMotorAnim::Roll,
            sprint_latched: false,
            sprint_exhausted: false,
            velocity_y: 0,
            grounded: false,
            ground_floor: 0,
            ground_anchor_x: 0,
            ground_anchor_z: 0,
        }
    }

    /// Reset position, yaw, stamina, and any in-progress action.
    pub fn snap_to(&mut self, position: RoomPoint, yaw: Angle) {
        self.position = position;
        self.yaw = yaw;
        self.stamina_q12 = DEFAULT_STAMINA_MAX_Q12;
        self.action = CharacterMotorAction::Idle;
        self.action_frame = 0;
        self.action_yaw = yaw;
        self.sprint_latched = false;
        self.sprint_exhausted = false;
        self.velocity_y = 0;
        self.grounded = false;
    }

    /// Move the motor to another coordinate space while preserving
    /// yaw, stamina, and any in-progress action. Used by streaming
    /// room transitions where the same physical player position is
    /// re-expressed relative to a newly-current chunk.
    pub fn relocate(&mut self, position: RoomPoint) {
        self.position = position;
        // Force the ground cache to re-resolve after a teleport / room switch:
        // the XZ cell and the active rooms may differ. Vertical velocity is
        // intentionally preserved so a fall continues across a room change.
        self.grounded = false;
    }

    /// Advance the motor by one frame.
    pub fn update(
        &mut self,
        collision: Option<RoomCollision<'_, '_>>,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
    ) -> CharacterMotorFrame {
        self.update_vblanks(collision, input, config, 1)
    }

    /// Advance the motor by elapsed display ticks.
    ///
    /// Heavy render paths can miss VBlanks. Animation already uses
    /// display time, so the motor catches up with small fixed
    /// substeps instead of scaling one large collision step. The cap
    /// prevents a long pause from spending a whole frame in movement
    /// catch-up.
    pub fn update_vblanks(
        &mut self,
        collision: Option<RoomCollision<'_, '_>>,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
    ) -> CharacterMotorFrame {
        self.update_vblanks_with_collision(
            CharacterCollision::room(collision),
            input,
            config,
            delta_vblanks,
        )
    }

    /// Advance the motor by elapsed display ticks with room and actor collision.
    pub fn update_vblanks_with_collision(
        &mut self,
        collision: CharacterCollision<'_, '_, '_>,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
    ) -> CharacterMotorFrame {
        let mut collision = GridCharacterCollision { collision };
        match self.update_vblanks_with_backend(&mut collision, input, config, delta_vblanks) {
            Ok(frame) => frame,
            Err(_) => unreachable!("grid collision queries are infallible"),
        }
    }

    /// Advance the motor through an allocation-free trace provider.
    ///
    /// The provider receives upright body traces using `config.radius` and
    /// `config.height`. On provider failure the complete motor state is restored
    /// and the error is returned; malformed BSP data or scratch exhaustion can
    /// therefore never commit a partial locomotion/action update.
    pub fn update_vblanks_with_trace_provider<P: CollisionTraceProvider + ?Sized>(
        &mut self,
        provider: &mut P,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
    ) -> Result<CharacterMotorFrame, CollisionQueryError> {
        let saved = *self;
        let mut collision = TraceCharacterCollision { provider };
        match self.update_vblanks_with_backend(&mut collision, input, config, delta_vblanks) {
            Ok(frame) => Ok(frame),
            Err(error) => {
                *self = saved;
                Err(error)
            }
        }
    }

    fn update_vblanks_with_backend<C: CharacterCollisionBackend>(
        &mut self,
        collision: &mut C,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
    ) -> Result<CharacterMotorFrame, CollisionQueryError> {
        let config = normalize_config(config);
        let steps = delta_vblanks.clamp(1, MAX_MOTOR_CATCHUP_VBLANKS);
        let mut final_frame: Option<CharacterMotorFrame> = None;

        for step in 0..steps {
            let mut step_input = input;
            if step > 0 {
                step_input.evade = false;
            }
            let frame = self.update_one_frame(collision, step_input, config)?;
            final_frame = Some(match final_frame {
                Some(mut aggregate) => {
                    aggregate.position = frame.position;
                    aggregate.yaw = frame.yaw;
                    aggregate.anim = frame.anim;
                    aggregate.action = frame.action;
                    aggregate.moved |= frame.moved;
                    aggregate.blocked |= frame.blocked;
                    aggregate.sprinting = frame.sprinting;
                    aggregate.invulnerable |= frame.invulnerable;
                    aggregate.recovery |= frame.recovery;
                    aggregate.stamina_q12 = frame.stamina_q12;
                    aggregate
                }
                None => frame,
            });
        }

        Ok(final_frame.unwrap_or_else(|| {
            self.frame(
                CharacterMotorAnim::Idle,
                self.action,
                false,
                false,
                false,
                false,
                false,
            )
        }))
    }

    fn update_one_frame<C: CharacterCollisionBackend>(
        &mut self,
        collision: &mut C,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
    ) -> Result<CharacterMotorFrame, CollisionQueryError> {
        self.stamina_q12 = self.stamina_q12.clamp(0, config.stamina_max_q12);
        self.apply_vertical(collision, config)?;

        if self.action.is_idle() && input.evade {
            self.try_start_evade(input, config);
        }

        if !self.action.is_idle() {
            return self.update_action(collision, config);
        }

        if let Some(facing_yaw) = input.facing_yaw {
            self.yaw = self.yaw.approach_q12(facing_yaw, config.yaw_step.as_q12());
        }

        if let Some((move_x, move_z, move_mag)) = analog_move_vector(input) {
            let move_yaw = yaw_from_vector(move_x, move_z);
            let directional_anim = if let Some(facing_yaw) = input.facing_yaw {
                locked_locomotion_anim(facing_yaw, move_yaw)
            } else {
                self.yaw = move_yaw;
                CharacterMotorAnim::Walk
            };
            // Lock-on holds the combat stance and, following Dark Souls,
            // allows sprint ONLY toward the target: you fast-walk sideways
            // relative to it or charge straight at it. Lateral and backward
            // movement therefore stay at walk speed on the walk-direction
            // clips, which is why the game needs no run-strafe animations at
            // all. Suppressing the sprint here also stops it draining stamina.
            let wants_sprint = input.sprint;
            self.update_sprint_gate(wants_sprint);
            let locked_lateral = input.facing_yaw.is_some_and(|facing_yaw| {
                !matches!(
                    locked_locomotion_anim(facing_yaw, move_yaw),
                    CharacterMotorAnim::Walk
                )
            });
            let sprinting = !locked_lateral && self.can_sprint(wants_sprint, config);
            let base_speed = if sprinting {
                config.run_speed
            } else {
                config.walk_speed
            };
            let speed = move_mag.mul_i32(base_speed);
            let directional_anim = match (sprinting, input.facing_yaw) {
                // Locked sprinting only happens toward the target, so the
                // forward run clip is the only one it can select.
                (true, Some(_)) => CharacterMotorAnim::Run,
                // Unlocked sprinting turns to face travel.
                (true, None) => {
                    self.yaw = move_yaw;
                    CharacterMotorAnim::Run
                }
                (false, _) => directional_anim,
            };
            let (moved, blocked) = self.try_move_vector(
                collision,
                move_x,
                move_z,
                speed,
                config.radius,
                config.height,
            )?;

            if sprinting && moved {
                self.spend_sprint_stamina(config);
            } else {
                self.recover_stamina(config);
            }

            // `directional_anim` already resolves the speed: plain `Run` when
            // sprinting unlocked, the matching directional run clip when
            // sprinting locked, and the walk-speed direction otherwise.
            let anim = if !moved && blocked {
                CharacterMotorAnim::Idle
            } else {
                directional_anim
            };

            return Ok(self.frame(
                anim,
                CharacterMotorAction::Idle,
                moved,
                blocked,
                sprinting,
                false,
                false,
            ));
        }

        if input.turn > 0 {
            self.yaw = self.yaw.add(config.yaw_step);
        } else if input.turn < 0 {
            self.yaw = self.yaw.sub(config.yaw_step);
        }

        let moving_intent = input.walk != 0;
        self.update_sprint_gate(input.sprint);
        let wants_forward_sprint = input.sprint && input.walk > 0;
        let sprinting = moving_intent && self.can_sprint(wants_forward_sprint, config);
        let speed = if sprinting {
            config.run_speed
        } else {
            config.walk_speed
        };
        let signed_speed = if input.walk < 0 { -speed } else { speed };

        let (moved, blocked) = if moving_intent {
            self.try_move(collision, signed_speed, config.radius, config.height)?
        } else {
            (false, false)
        };

        if sprinting && moved {
            self.spend_sprint_stamina(config);
        } else {
            self.recover_stamina(config);
        }

        let anim = if !moving_intent || !moved && blocked {
            CharacterMotorAnim::Idle
        } else if sprinting {
            CharacterMotorAnim::Run
        } else {
            CharacterMotorAnim::Walk
        };

        Ok(self.frame(
            anim,
            CharacterMotorAction::Idle,
            moved,
            blocked,
            sprinting,
            false,
            false,
        ))
    }

    /// Current root position.
    pub const fn position(&self) -> RoomPoint {
        self.position
    }

    /// Current facing yaw.
    pub const fn yaw(&self) -> Angle {
        self.yaw
    }

    /// Current stamina value.
    pub const fn stamina_q12(&self) -> i32 {
        self.stamina_q12
    }

    /// Current fixed action.
    pub const fn action(&self) -> CharacterMotorAction {
        self.action
    }

    /// True while the current fixed action grants invulnerability
    /// (the Souls i-frame window: `roll_invulnerable_frames`, plus the
    /// compatibility quickstep profile when explicitly driven downstream).
    /// Queried BEFORE this tick's motor update it reports
    /// exactly the invulnerability the update will apply, so combat
    /// resolution that runs earlier in the tick agrees with the
    /// motor's own frame result. Idle is never invulnerable.
    pub fn is_action_invulnerable(&self, config: CharacterMotorConfig) -> bool {
        let profile = ActionProfile::for_action(self.action, normalize_config(config));
        self.action_frame < profile.invulnerable_frames
    }

    fn try_start_evade(&mut self, input: CharacterMotorInput, config: CharacterMotorConfig) {
        let analog = analog_move_vector(input);
        let action = CharacterMotorAction::Roll;
        if let Some((move_x, move_z, _)) = analog {
            self.action_yaw = yaw_from_vector(move_x, move_z);
            if let Some(facing_yaw) = input.facing_yaw {
                // Lock-on evade: slide in the input direction while the
                // body keeps facing the target. The clip follows the
                // slide direction (forward = the Roll slot, backward =
                // the Backstep slot, sides = the dash slots).
                self.action_anim = locked_evade_anim(facing_yaw, self.action_yaw);
            } else {
                self.yaw = self.action_yaw;
                self.action_anim = CharacterMotorAnim::Roll;
            }
        } else {
            self.action_anim = CharacterMotorAnim::Roll;
            // With no directional input, preserve a responsive evade button:
            // move forward along the current combat/free-movement facing.
            self.action_yaw = input.facing_yaw.unwrap_or_else(|| {
                if input.walk < 0 {
                    self.yaw.add(Angle::HALF)
                } else {
                    self.yaw
                }
            });
            self.yaw = self.action_yaw;
        }
        let cost = match action {
            CharacterMotorAction::Idle => 0,
            CharacterMotorAction::Roll => config.roll_cost_q12,
            CharacterMotorAction::Quickstep => config.backstep_cost_q12,
        };
        if self.stamina_q12 < cost {
            return;
        }
        self.stamina_q12 -= cost;
        self.action = action;
        self.action_frame = 0;
    }

    fn update_action<C: CharacterCollisionBackend>(
        &mut self,
        collision: &mut C,
        config: CharacterMotorConfig,
    ) -> Result<CharacterMotorFrame, CollisionQueryError> {
        let profile = ActionProfile::for_action(self.action, config);
        let frame = self.action_frame;
        let active = frame < profile.active_frames;
        let invulnerable = frame < profile.invulnerable_frames;
        let recovery = frame >= profile.active_frames;

        let (moved, blocked) = if active {
            let signed_speed = profile.speed.saturating_mul(profile.direction as i32);
            self.try_move_at_yaw(
                collision,
                self.action_yaw,
                signed_speed,
                config.radius,
                config.height,
            )?
        } else {
            (false, false)
        };

        self.action_frame = self.action_frame.saturating_add(1);
        let finished = self.action_frame >= profile.total_frames();
        let action = self.action;
        if finished {
            self.action = CharacterMotorAction::Idle;
            self.action_frame = 0;
            self.recover_stamina(config);
        }

        let anim = match action {
            CharacterMotorAction::Idle => CharacterMotorAnim::Idle,
            CharacterMotorAction::Roll => self.action_anim,
            CharacterMotorAction::Quickstep => CharacterMotorAnim::Quickstep,
        };
        Ok(self.frame(anim, action, moved, blocked, false, invulnerable, recovery))
    }

    fn try_move<C: CharacterCollisionBackend>(
        &mut self,
        collision: &mut C,
        signed_speed: i32,
        radius: i32,
        height: i32,
    ) -> Result<(bool, bool), CollisionQueryError> {
        self.try_move_at_yaw(collision, self.yaw, signed_speed, radius, height)
    }

    fn try_move_at_yaw<C: CharacterCollisionBackend>(
        &mut self,
        collision: &mut C,
        yaw: Angle,
        signed_speed: i32,
        radius: i32,
        height: i32,
    ) -> Result<(bool, bool), CollisionQueryError> {
        if signed_speed == 0 {
            return Ok((false, false));
        }
        let sin_yaw = yaw.sin();
        let cos_yaw = yaw.cos();
        let target = RoomPoint::new(
            self.position
                .x
                .saturating_add(sin_yaw.mul_i32(signed_speed)),
            self.position.y,
            self.position
                .z
                .saturating_add(cos_yaw.mul_i32(signed_speed)),
        );
        self.try_commit_move(collision, target, radius, height)
    }

    fn try_move_vector<C: CharacterCollisionBackend>(
        &mut self,
        collision: &mut C,
        move_x: Q12,
        move_z: Q12,
        speed: i32,
        radius: i32,
        height: i32,
    ) -> Result<(bool, bool), CollisionQueryError> {
        if speed == 0 {
            return Ok((false, false));
        }
        let dx = move_x.mul_i32(speed);
        let dz = move_z.mul_i32(speed);
        if dx == 0 && dz == 0 {
            return Ok((false, false));
        }
        let target = RoomPoint::new(
            self.position.x.saturating_add(dx),
            self.position.y,
            self.position.z.saturating_add(dz),
        );
        self.try_commit_move(collision, target, radius, height)
    }

    fn try_commit_move<C: CharacterCollisionBackend>(
        &mut self,
        collision: &mut C,
        target: RoomPoint,
        radius: i32,
        height: i32,
    ) -> Result<(bool, bool), CollisionQueryError> {
        let shape = CollisionTraceShape::Body { radius, height };
        if let Some(position) = collision.stand_position(self.position, target, shape)? {
            self.position = position;
            return Ok((true, false));
        }

        let start = self.position;
        let x_only = RoomPoint::new(target.x, start.y, start.z);
        if let Some(position) = collision.stand_position(start, x_only, shape)? {
            self.position = position;
            return Ok((position.x != start.x || position.z != start.z, true));
        }

        let z_only = RoomPoint::new(start.x, start.y, target.z);
        if let Some(position) = collision.stand_position(start, z_only, shape)? {
            self.position = position;
            return Ok((position.x != start.x || position.z != start.z, true));
        }

        // `apply_vertical` validated and anchored this exact X/Z at the start
        // of the tick. When every candidate is blocked, keep that known-good
        // grounded position instead of repeating two full floor/wall scans.
        // The recovery probes below remain for airborne/no-floor edge cases.
        if self.grounded {
            return Ok((false, true));
        }

        if collision.stand_position(start, start, shape)?.is_some() {
            return Ok((false, true));
        }

        if !collision.has_world_collision() {
            self.position = target;
            return Ok((true, false));
        }

        if target == start {
            return Ok((false, false));
        }
        let Some(position) = collision.recovery_position(start, shape)? else {
            return Ok((false, true));
        };
        self.position = position;
        Ok((false, true))
    }

    fn recover_stamina(&mut self, config: CharacterMotorConfig) {
        self.stamina_q12 = self
            .stamina_q12
            .saturating_add(config.stamina_recover_q12)
            .min(config.stamina_max_q12);
    }

    fn update_sprint_gate(&mut self, wants_sprint: bool) {
        if !wants_sprint {
            self.sprint_latched = false;
            self.sprint_exhausted = false;
        }
    }

    fn can_sprint(&mut self, wants_sprint: bool, config: CharacterMotorConfig) -> bool {
        if !wants_sprint {
            return false;
        }
        if self.sprint_exhausted || self.stamina_q12 <= 0 {
            self.sprint_latched = false;
            return false;
        }
        if self.sprint_latched || self.stamina_q12 >= config.sprint_min_q12 {
            self.sprint_latched = true;
            true
        } else {
            false
        }
    }

    fn spend_sprint_stamina(&mut self, config: CharacterMotorConfig) {
        self.stamina_q12 = self
            .stamina_q12
            .saturating_sub(config.sprint_drain_q12)
            .max(0);
        if self.stamina_q12 == 0 {
            self.sprint_latched = false;
            self.sprint_exhausted = true;
        }
    }

    /// Vertical update run once per fixed tick. Keeps the feet glued to the
    /// supporting floor when grounded, and integrates gravity when the body
    /// is airborne over a ledge or hole so it falls rather than teleporting
    /// down. Replaces the old unconditional floor snap.
    ///
    /// Probing only the centre column is intentional: a prior move already
    /// validated the cylinder footprint, so for the per-tick settle just the
    /// centre floor height is needed to stay grounded on slopes and steps.
    fn apply_vertical<C: CharacterCollisionBackend>(
        &mut self,
        collision: &mut C,
        config: CharacterMotorConfig,
    ) -> Result<(), CollisionQueryError> {
        // Grounded fast path: a body that is grounded and has not moved in XZ
        // sits on the same (static) floor as last tick, so reuse the cached
        // height and skip the multi-room ground query (no divide, no room
        // iteration, no interpolation). This is the common case for the many
        // idle entities that will each run this every tick.
        if self.grounded
            && self.position.x == self.ground_anchor_x
            && self.position.z == self.ground_anchor_z
        {
            self.position.y = self.ground_floor;
            self.velocity_y = 0;
            return Ok(());
        }

        // Cold path: resolve the supporting floor (highest floor at/below the
        // feet plus a step, across the active rooms).
        let shape = CollisionTraceShape::Body {
            radius: config.radius,
            height: config.height,
        };
        let Some(floor) = collision.supporting_floor(self.position, shape)? else {
            // No floor anywhere below (open void): hold rather than fall
            // forever. Matches the legacy no-room behaviour.
            self.grounded = false;
            self.velocity_y = 0;
            return Ok(());
        };

        if self.position.y.saturating_sub(floor) <= STEP_DOWN_HEIGHT {
            // On the floor, or within a step of it: snap down and ground.
            // Caching the cell lets the next idle tick take the fast path.
            self.position.y = floor;
            self.velocity_y = 0;
            self.set_grounded(floor);
            return Ok(());
        }

        // Airborne: the floor is more than a step below (a ledge or hole).
        // Accelerate downward (clamped to terminal) and move, landing exactly
        // on the floor without overshooting through it.
        self.grounded = false;
        let gravity = config
            .gravity_per_tick
            .saturating_mul(config.weight_q8 as i32)
            / DEFAULT_WEIGHT_Q8 as i32;
        self.velocity_y = self.velocity_y.saturating_sub(gravity).max(-MAX_FALL_SPEED);
        let next = self.position.y.saturating_add(self.velocity_y);
        if next <= floor {
            self.position.y = floor;
            self.velocity_y = 0;
            self.set_grounded(floor);
        } else {
            self.position.y = next;
        }
        Ok(())
    }

    /// Mark the body grounded on `floor` and anchor the ground cache to the
    /// current XZ so the next stationary tick takes the fast path.
    fn set_grounded(&mut self, floor: i32) {
        self.grounded = true;
        self.ground_floor = floor;
        self.ground_anchor_x = self.position.x;
        self.ground_anchor_z = self.position.z;
    }

    fn frame(
        &self,
        anim: CharacterMotorAnim,
        action: CharacterMotorAction,
        moved: bool,
        blocked: bool,
        sprinting: bool,
        invulnerable: bool,
        recovery: bool,
    ) -> CharacterMotorFrame {
        CharacterMotorFrame {
            position: self.position,
            yaw: self.yaw,
            anim,
            action,
            moved,
            blocked,
            sprinting,
            invulnerable,
            recovery,
            stamina_q12: self.stamina_q12,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ActionProfile {
    speed: i32,
    direction: i8,
    active_frames: u8,
    recovery_frames: u8,
    invulnerable_frames: u8,
}

impl ActionProfile {
    fn for_action(action: CharacterMotorAction, config: CharacterMotorConfig) -> Self {
        match action {
            CharacterMotorAction::Idle => Self {
                speed: 0,
                direction: 0,
                active_frames: 0,
                recovery_frames: 0,
                invulnerable_frames: 0,
            },
            CharacterMotorAction::Roll => Self {
                speed: config.roll_speed,
                direction: 1,
                active_frames: config.roll_active_frames,
                recovery_frames: config.roll_recovery_frames,
                invulnerable_frames: config.roll_invulnerable_frames,
            },
            CharacterMotorAction::Quickstep => Self {
                speed: config.backstep_speed,
                direction: 1,
                active_frames: config.backstep_active_frames,
                recovery_frames: config.backstep_recovery_frames,
                invulnerable_frames: config.backstep_invulnerable_frames,
            },
        }
    }

    fn total_frames(self) -> u8 {
        self.active_frames
            .saturating_add(self.recovery_frames)
            .max(1)
    }
}

fn normalize_config(mut config: CharacterMotorConfig) -> CharacterMotorConfig {
    config.radius = config.radius.max(0);
    config.height = config.height.max(1);
    config.walk_speed = config.walk_speed.max(0);
    config.run_speed = config.run_speed.max(config.walk_speed);
    if config.yaw_step == Angle::ZERO {
        config.yaw_step = Angle::from_q12(1);
    }
    config.gravity_per_tick = config.gravity_per_tick.max(0);
    config.weight_q8 = config.weight_q8.clamp(MIN_WEIGHT_Q8, MAX_WEIGHT_Q8);
    config.stamina_max_q12 = config.stamina_max_q12.max(1);
    config.sprint_min_q12 = config.sprint_min_q12.clamp(0, config.stamina_max_q12);
    config.sprint_drain_q12 = config.sprint_drain_q12.max(0);
    config.stamina_recover_q12 = config.stamina_recover_q12.max(0);
    config.roll_cost_q12 = config.roll_cost_q12.clamp(0, config.stamina_max_q12);
    config.roll_speed = config.roll_speed.max(0);
    config.roll_active_frames = config.roll_active_frames.max(1);
    config.roll_invulnerable_frames = config.roll_invulnerable_frames.min(
        config
            .roll_active_frames
            .saturating_add(config.roll_recovery_frames),
    );
    config.backstep_cost_q12 = config.backstep_cost_q12.clamp(0, config.stamina_max_q12);
    config.backstep_speed = config.backstep_speed.max(0);
    config.backstep_active_frames = config.backstep_active_frames.max(1);
    config.backstep_invulnerable_frames = config.backstep_invulnerable_frames.min(
        config
            .backstep_active_frames
            .saturating_add(config.backstep_recovery_frames),
    );
    config
}

fn trace_supporting_floor<P: CollisionTraceProvider + ?Sized>(
    provider: &mut P,
    position: RoomPoint,
    shape: CollisionTraceShape,
) -> Result<Option<i32>, CollisionQueryError> {
    let start = position.with_y(position.y.saturating_add(TRACE_FLOOR_PROBE_LIFT));
    let end = position.with_y(position.y.saturating_sub(TRACE_FLOOR_PROBE_DOWN));
    let trace = trace_collision(provider, CollisionTraceQuery { start, end, shape })?;
    if trace.start_solid
        || trace.all_solid
        || trace.fraction_q12 >= COLLISION_FRACTION_ONE_Q12
        || trace.normal_q12[1] <= 0
    {
        return Ok(None);
    }
    Ok(Some(trace.end.y))
}

fn trace_stand_position<P: CollisionTraceProvider + ?Sized>(
    provider: &mut P,
    start: RoomPoint,
    target: RoomPoint,
    shape: CollisionTraceShape,
) -> Result<Option<RoomPoint>, CollisionQueryError> {
    let direct = trace_collision(
        provider,
        CollisionTraceQuery {
            start,
            end: target,
            shape,
        },
    )?;
    if !direct.hit() {
        let Some(floor) = trace_supporting_floor(provider, target, shape)? else {
            return Ok(None);
        };
        if floor > target.y.saturating_add(STEP_UP_HEIGHT) {
            return Ok(None);
        }
        return Ok(Some(target.with_y(resolve_step_down(target.y, floor))));
    }

    // A direct body sweep that hits a low riser gets one bounded step attempt:
    // lift, sweep at the raised height, then settle back onto an upward-facing
    // floor. Tall walls/ceilings reject either the lift or raised sweep.
    let raised_start = start.with_y(start.y.saturating_add(STEP_UP_HEIGHT));
    let lift = trace_collision(
        provider,
        CollisionTraceQuery {
            start,
            end: raised_start,
            shape,
        },
    )?;
    if lift.hit() {
        return Ok(None);
    }
    let raised_target = target.with_y(raised_start.y);
    let across = trace_collision(
        provider,
        CollisionTraceQuery {
            start: raised_start,
            end: raised_target,
            shape,
        },
    )?;
    if across.hit() {
        return Ok(None);
    }
    let settle_end = target.with_y(target.y.saturating_sub(STEP_DOWN_HEIGHT));
    let settle = trace_collision(
        provider,
        CollisionTraceQuery {
            start: raised_target,
            end: settle_end,
            shape,
        },
    )?;
    if settle.start_solid
        || settle.all_solid
        || settle.fraction_q12 >= COLLISION_FRACTION_ONE_Q12
        || settle.normal_q12[1] <= 0
    {
        return Ok(None);
    }
    Ok(Some(settle.end))
}

/// Resolve the supporting floor height under `(x, z)` in the motor's
/// current space, preferring the multi-room (streaming) collision and
/// falling back to a single room. `None` means no floor anywhere below
/// (an open void), in which case the caller holds position.
fn supporting_floor_height(
    collision: &CharacterCollision<'_, '_, '_>,
    x: i32,
    z: i32,
    feet_y: i32,
) -> Option<i32> {
    if !collision.rooms.is_empty() {
        supporting_floor_in_rooms(collision.rooms, x, z, feet_y)
    } else if let Some(room) = collision.room {
        floor_height_at(room, x, z)
    } else {
        None
    }
}

/// Feet height when moving onto a cell whose floor is `floor`. Snap to the
/// floor for steps up and small steps down, but for a drop deeper than
/// [`STEP_DOWN_HEIGHT`] keep the feet at their current height so the body
/// walks out over the ledge; [`CharacterMotorState::apply_vertical`] then
/// makes it fall instead of teleporting down.
fn resolve_step_down(feet_y: i32, floor: i32) -> i32 {
    if floor < feet_y.saturating_sub(STEP_DOWN_HEIGHT) {
        feet_y
    } else {
        floor
    }
}

fn body_stand_position(
    collision: CharacterCollision<'_, '_, '_>,
    target: RoomPoint,
    radius: i32,
    height: i32,
) -> Option<RoomPoint> {
    let radius = radius.max(0);
    let height = height.max(1);
    let position = if !collision.rooms.is_empty() {
        let floor = stand_height_in_rooms(collision.rooms, target.x, target.z, radius, target.y)?;
        let position = target.with_y(resolve_step_down(target.y, floor));
        if body_hits_solid_wall_in_rooms(collision.rooms, position, radius, height) {
            return None;
        }
        position
    } else {
        match collision.room {
            Some(room) => {
                let floor = stand_height(room, target.x, target.z, radius)?;
                let position = target.with_y(resolve_step_down(target.y, floor));
                if body_hits_solid_wall(room, position, radius, height) {
                    return None;
                }
                position
            }
            None => target,
        }
    };
    if body_hits_blocker(position, radius, height, collision.blockers)
        || body_hits_aabb_blocker(position, radius, height, collision.aabb_blockers)
    {
        return None;
    }
    Some(position)
}

/// Outcome of one AI-body walk step through [`commit_body_step`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BodyStep {
    /// Committed position after the step (== the start when blocked).
    pub position: RoomPoint,
    /// Whether the body changed X/Z position.
    pub moved: bool,
    /// Whether any axis of the requested step was rejected.
    pub blocked: bool,
}

/// One collision-checked walk step for a non-player body cylinder (the
/// game-entity runtime's movement primitive, phase 3 of
/// docs/game-runtime-plan.md). Attempts `start + (dx, 0, dz)` with the
/// same stand test / axis-slide cascade the player motor's move commit
/// uses ([`body_stand_position`]: grid floor lookup, walkable-footprint
/// samples, [`STEP_UP_HEIGHT`]/[`STEP_DOWN_HEIGHT`] step rules, wall and
/// blocker rejection), so entities obey exactly the collision rules the
/// player does.
///
/// One deliberate difference from the player: entities do not fall in
/// this slice. Where the player walks out over a deep ledge and
/// `apply_vertical` drops them, an AI step whose destination floor is
/// more than [`STEP_DOWN_HEIGHT`] below the feet is REJECTED (the axis
/// slide still applies), so patrol/chase paths never leave the walkable
/// grid. Gravity for thrown/falling entities is the combat slice's
/// work.
pub fn commit_body_step(
    collision: CharacterCollision<'_, '_, '_>,
    start: RoomPoint,
    dx: i32,
    dz: i32,
    radius: i32,
    height: i32,
) -> BodyStep {
    let target = RoomPoint::new(
        start.x.saturating_add(dx),
        start.y,
        start.z.saturating_add(dz),
    );
    if target.x == start.x && target.z == start.z {
        return BodyStep {
            position: start,
            moved: false,
            blocked: false,
        };
    }

    if let Some(position) = body_grounded_stand_position(collision, target, radius, height) {
        return BodyStep {
            position,
            moved: true,
            blocked: false,
        };
    }

    // Blocked on the full step: slide along the free axis, exactly the
    // player commit's cascade order (X first, then Z).
    let x_only = RoomPoint::new(target.x, start.y, start.z);
    if let Some(position) = body_grounded_stand_position(collision, x_only, radius, height) {
        return BodyStep {
            position,
            moved: position.x != start.x || position.z != start.z,
            blocked: true,
        };
    }
    let z_only = RoomPoint::new(start.x, start.y, target.z);
    if let Some(position) = body_grounded_stand_position(collision, z_only, radius, height) {
        return BodyStep {
            position,
            moved: position.x != start.x || position.z != start.z,
            blocked: true,
        };
    }
    BodyStep {
        position: start,
        moved: false,
        blocked: true,
    }
}

/// Collision-check one non-player body step through a trace provider.
///
/// This is the BSP/provider counterpart of [`commit_body_step`]. It keeps the
/// same deterministic full-step, X-only, then Z-only cascade while sharing the
/// exact body/floor trace rules used by [`CharacterMotorState`]. A provider
/// failure is explicit and never commits a partial step.
pub fn commit_body_step_with_trace_provider<P: CollisionTraceProvider + ?Sized>(
    provider: &mut P,
    start: RoomPoint,
    dx: i32,
    dz: i32,
    radius: i32,
    height: i32,
) -> Result<BodyStep, CollisionQueryError> {
    let target = RoomPoint::new(
        start.x.saturating_add(dx),
        start.y,
        start.z.saturating_add(dz),
    );
    if target.x == start.x && target.z == start.z {
        return Ok(BodyStep {
            position: start,
            moved: false,
            blocked: false,
        });
    }
    let shape = CollisionTraceShape::Body {
        radius: radius.max(0),
        height: height.max(1),
    };
    if let Some(position) = trace_body_grounded_stand_position(provider, start, target, shape)? {
        return Ok(BodyStep {
            position,
            moved: true,
            blocked: false,
        });
    }

    let x_only = RoomPoint::new(target.x, start.y, start.z);
    if let Some(position) = trace_body_grounded_stand_position(provider, start, x_only, shape)? {
        return Ok(BodyStep {
            position,
            moved: position.x != start.x || position.z != start.z,
            blocked: true,
        });
    }
    let z_only = RoomPoint::new(start.x, start.y, target.z);
    if let Some(position) = trace_body_grounded_stand_position(provider, start, z_only, shape)? {
        return Ok(BodyStep {
            position,
            moved: position.x != start.x || position.z != start.z,
            blocked: true,
        });
    }
    Ok(BodyStep {
        position: start,
        moved: false,
        blocked: true,
    })
}

fn trace_body_grounded_stand_position<P: CollisionTraceProvider + ?Sized>(
    provider: &mut P,
    start: RoomPoint,
    target: RoomPoint,
    shape: CollisionTraceShape,
) -> Result<Option<RoomPoint>, CollisionQueryError> {
    let Some(position) = trace_stand_position(provider, start, target, shape)? else {
        return Ok(None);
    };
    let Some(floor) = trace_supporting_floor(provider, position, shape)? else {
        return Ok(None);
    };
    if position.y.saturating_sub(floor) > STEP_DOWN_HEIGHT {
        return Ok(None);
    }
    Ok(Some(position.with_y(floor)))
}

/// [`body_stand_position`] plus the AI grounding rule: the committed
/// spot must have supporting floor within [`STEP_DOWN_HEIGHT`] of the
/// feet (`body_stand_position` holds the feet height over deeper
/// drops for the player's fall path; for AI that reads as "off the
/// walkable grid" and rejects the candidate).
fn body_grounded_stand_position(
    collision: CharacterCollision<'_, '_, '_>,
    target: RoomPoint,
    radius: i32,
    height: i32,
) -> Option<RoomPoint> {
    let position = body_stand_position(collision, target, radius, height)?;
    if collision.rooms.is_empty() && collision.room.is_none() {
        // No room collision wired (unit-test/no-clip shapes): trust it.
        return Some(position);
    }
    let floor = supporting_floor_height(&collision, position.x, position.z, position.y)?;
    if position.y.saturating_sub(floor) > STEP_DOWN_HEIGHT {
        return None;
    }
    Some(position.with_y(floor))
}

fn stand_height_in_rooms(
    rooms: &[CharacterCollisionRoom<'_>],
    x: i32,
    z: i32,
    radius: i32,
    feet_y: i32,
) -> Option<i32> {
    let height = supporting_floor_in_rooms(rooms, x, z, feet_y)?;
    if radius <= 0 {
        return Some(height);
    }
    let r = radius.max(0);
    let footprint_clear = floor_walkable_at_rooms(rooms, x.saturating_sub(r), z)
        && floor_walkable_at_rooms(rooms, x.saturating_add(r), z)
        && floor_walkable_at_rooms(rooms, x, z.saturating_sub(r))
        && floor_walkable_at_rooms(rooms, x, z.saturating_add(r));
    footprint_clear.then_some(height)
}

#[cfg(test)]
fn floor_height_at_rooms(rooms: &[CharacterCollisionRoom<'_>], x: i32, z: i32) -> Option<i32> {
    for collision_room in rooms {
        let Some(room) = collision_room.room else {
            continue;
        };
        if let Some(height) = floor_height_at_collision_room(*collision_room, room, x, z) {
            return Some(height);
        }
    }
    None
}

/// Highest floor at `(x, z)` the feet can rest on: the tallest floor across
/// ALL collision rooms that sits at or below `feet_y + STEP_UP_HEIGHT`
/// (a step up is reachable; anything higher is a wall or ceiling). This
/// picks the floor by elevation, not by room order -- essential for stacked
/// floors, where the player standing on an upper floor must rest on it
/// rather than be pulled down to the lower floor occupying the same X/Z.
/// Returns `None` when the only floors lie above reach (open space above
/// the feet).
fn supporting_floor_in_rooms(
    rooms: &[CharacterCollisionRoom<'_>],
    x: i32,
    z: i32,
    feet_y: i32,
) -> Option<i32> {
    let reach = feet_y.saturating_add(STEP_UP_HEIGHT);
    let mut best: Option<i32> = None;
    for collision_room in rooms {
        let Some(room) = collision_room.room else {
            continue;
        };
        if let Some(height) = floor_height_at_collision_room(*collision_room, room, x, z) {
            if height <= reach {
                best = Some(best.map_or(height, |b| b.max(height)));
            }
        }
    }
    best
}

/// Multi-room walkability check used by the cylinder-footprint samples.
fn floor_walkable_at_rooms(rooms: &[CharacterCollisionRoom<'_>], x: i32, z: i32) -> bool {
    for collision_room in rooms {
        let Some(room) = collision_room.room else {
            continue;
        };
        if floor_walkable_at_collision_room(*collision_room, room, x, z) {
            return true;
        }
    }
    false
}

fn floor_height_at_collision_room(
    collision_room: CharacterCollisionRoom<'_>,
    room: RuntimeCollisionRoom<'_>,
    x: i32,
    z: i32,
) -> Option<i32> {
    floor_height_at(
        room.collision(),
        x.saturating_sub(collision_room.offset_x),
        z.saturating_sub(collision_room.offset_z),
    )
    .map(|height| height.saturating_add(collision_room.offset_y))
}

fn floor_walkable_at_collision_room(
    collision_room: CharacterCollisionRoom<'_>,
    room: RuntimeCollisionRoom<'_>,
    x: i32,
    z: i32,
) -> bool {
    floor_walkable_at(
        room.collision(),
        x.saturating_sub(collision_room.offset_x),
        z.saturating_sub(collision_room.offset_z),
    )
}

fn body_hits_solid_wall_in_rooms(
    rooms: &[CharacterCollisionRoom<'_>],
    position: RoomPoint,
    radius: i32,
    height: i32,
) -> bool {
    for collision_room in rooms {
        let Some(room) = collision_room.room else {
            continue;
        };
        if !collision_room_contains_point(*collision_room, room, position.x, position.z) {
            continue;
        }
        let local_position = RoomPoint::new(
            position.x.saturating_sub(collision_room.offset_x),
            position.y.saturating_sub(collision_room.offset_y),
            position.z.saturating_sub(collision_room.offset_z),
        );
        if body_hits_solid_wall(room.collision(), local_position, radius, height) {
            return true;
        }
    }
    false
}

fn collision_room_contains_point(
    collision_room: CharacterCollisionRoom<'_>,
    room: RuntimeCollisionRoom<'_>,
    x: i32,
    z: i32,
) -> bool {
    let Some((x0, x1, z0, z1)) = collision_room_bounds(collision_room, room) else {
        return false;
    };
    if x < x0 || x >= x1 || z < z0 || z >= z1 {
        return false;
    }
    let sector_size = room.sector_size();
    if sector_size <= 0 {
        return false;
    }
    let sx = (x.saturating_sub(x0) / sector_size) as u16;
    let sz = (z.saturating_sub(z0) / sector_size) as u16;
    room.collision().sector_probe(sx, sz).is_some()
}

fn collision_room_bounds(
    collision_room: CharacterCollisionRoom<'_>,
    room: RuntimeCollisionRoom<'_>,
) -> Option<(i32, i32, i32, i32)> {
    let sector_size = room.sector_size();
    if sector_size <= 0 {
        return None;
    }
    let x0 = collision_room.offset_x;
    let z0 = collision_room.offset_z;
    let x1 = x0.checked_add((room.width() as i32).checked_mul(sector_size)?)?;
    let z1 = z0.checked_add((room.depth() as i32).checked_mul(sector_size)?)?;
    Some((x0, x1, z0, z1))
}

fn analog_move_vector(input: CharacterMotorInput) -> Option<(Q12, Q12, Q12)> {
    let x = input.move_x.raw();
    let z = input.move_z.raw();
    if x == 0 && z == 0 {
        return None;
    }
    let mag = isqrt_i32(square_i32_saturating(x).saturating_add(square_i32_saturating(z)));
    if mag <= 0 {
        return None;
    }
    if mag <= Q12::SCALE {
        return Some((input.move_x, input.move_z, Q12::from_raw(mag)));
    }
    Some((
        Q12::ONE.mul_ratio(x, mag),
        Q12::ONE.mul_ratio(z, mag),
        Q12::ONE,
    ))
}

fn yaw_from_vector(dx: Q12, dz: Q12) -> Angle {
    let dx = dx.raw();
    let dz = dz.raw();
    if dx == 0 && dz == 0 {
        return Angle::ZERO;
    }
    let ax = abs_i32(dx);
    let az = abs_i32(dz);
    let base = if ax <= az {
        ax * 512 / az.max(1)
    } else {
        1024 - (az * 512 / ax.max(1))
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

/// Locked-on evade clip choice by slide direction relative to facing.
/// Sector convention matches [`locked_locomotion_anim`]; forward maps
/// to the Roll slot and backward to the Backstep (quickstep) slot, the
/// slots the four directional slide clips bind to.
fn locked_evade_anim(facing_yaw: Angle, move_yaw: Angle) -> CharacterMotorAnim {
    let delta = facing_yaw.shortest_delta_q12(move_yaw);
    let abs_delta = i32::from(delta).abs();
    if abs_delta <= 512 {
        CharacterMotorAnim::Roll
    } else if abs_delta >= 1536 {
        CharacterMotorAnim::Quickstep
    } else if delta < 0 {
        CharacterMotorAnim::DashLeft
    } else {
        CharacterMotorAnim::DashRight
    }
}

fn locked_locomotion_anim(facing_yaw: Angle, move_yaw: Angle) -> CharacterMotorAnim {
    let delta = facing_yaw.shortest_delta_q12(move_yaw);
    let abs_delta = i32::from(delta).abs();
    if abs_delta <= 512 {
        CharacterMotorAnim::Walk
    } else if abs_delta >= 1536 {
        CharacterMotorAnim::WalkBackward
    } else if delta < 0 {
        CharacterMotorAnim::StrafeLeft
    } else {
        CharacterMotorAnim::StrafeRight
    }
}


fn stand_height(room: RoomCollision<'_, '_>, x: i32, z: i32, radius: i32) -> Option<i32> {
    let height = floor_height_at(room, x, z)?;
    if radius <= 0 {
        return Some(height);
    }
    let r = radius.max(0);
    let footprint_clear = floor_walkable_at(room, x.saturating_sub(r), z)
        && floor_walkable_at(room, x.saturating_add(r), z)
        && floor_walkable_at(room, x, z.saturating_sub(r))
        && floor_walkable_at(room, x, z.saturating_add(r));
    footprint_clear.then_some(height)
}

fn floor_probe(room: RoomCollision<'_, '_>, x: i32, z: i32, need_height: bool) -> Option<i32> {
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
    if !sector.walkable() {
        return None;
    }
    if !need_height {
        return Some(0);
    }
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

/// Interpolated floor height at a point, or `None` if it is off walkable floor.
fn floor_height_at(room: RoomCollision<'_, '_>, x: i32, z: i32) -> Option<i32> {
    floor_probe(room, x, z, true)
}

/// Whether a point sits on walkable floor, without interpolating its height.
/// The four cylinder-footprint samples in `stand_height` only need this; skipping
/// the interpolation drops `triangle_heights_to_quad` + `height_at_local` (and its
/// per-axis divides) for four of every five floor queries.
fn floor_walkable_at(room: RoomCollision<'_, '_>, x: i32, z: i32) -> bool {
    floor_probe(room, x, z, false).is_some()
}

fn body_hits_solid_wall(
    room: RoomCollision<'_, '_>,
    position: RoomPoint,
    radius: i32,
    height: i32,
) -> bool {
    if radius <= 0 {
        return false;
    }
    let s = room.sector_size();
    if s <= 0 {
        return true;
    }
    let min_sx = (position.x.saturating_sub(radius).max(0) / s)
        .saturating_sub(1)
        .max(0);
    let max_sx = (position.x.saturating_add(radius).max(0) / s).saturating_add(1);
    let min_sz = (position.z.saturating_sub(radius).max(0) / s)
        .saturating_sub(1)
        .max(0);
    let max_sz = (position.z.saturating_add(radius).max(0) / s).saturating_add(1);
    let mut sx = min_sx;
    while sx <= max_sx && sx < room.width() as i32 {
        let mut sz = min_sz;
        while sz <= max_sz && sz < room.depth() as i32 {
            if let Some(sector) = room.sector_probe(sx as u16, sz as u16) {
                let mut i = 0;
                while i < sector.wall_count() {
                    if let Some(wall) = room.sector_probe_wall(sector, i) {
                        if wall.solid()
                            && wall_blocks_body(position.y, height, wall.heights())
                            && circle_overlaps_wall_segment(
                                position.x,
                                position.z,
                                radius,
                                sx,
                                sz,
                                s,
                                wall.direction(),
                            )
                        {
                            return true;
                        }
                    }
                    i += 1;
                }
            }
            sz += 1;
        }
        sx += 1;
    }
    false
}

/// Whether a solid wall blocks the body, accounting for stair-stepping.
/// A wall blocks when it overlaps the body's vertical span AND its top
/// rises more than [`STEP_UP_HEIGHT`] above the body's feet. A lower
/// riser is a step the character climbs: the floor probe has already
/// confirmed walkable floor at the target X/Z, so the body rises onto it
/// rather than being stopped. `apply_vertical` then settles the feet on the
/// step surface the next tick.
fn wall_blocks_body(feet_y: i32, body_height: i32, wall_heights: [i32; 4]) -> bool {
    if !vertical_ranges_overlap(feet_y, body_height, wall_heights) {
        return false;
    }
    let wall_top = wall_heights.iter().copied().max().unwrap_or(feet_y);
    // Steppable riser: top within a step of the feet -> not a blocker.
    wall_top > feet_y.saturating_add(STEP_UP_HEIGHT)
}

fn vertical_ranges_overlap(body_y: i32, body_height: i32, wall_heights: [i32; 4]) -> bool {
    let body_min = body_y;
    let body_max = body_y.saturating_add(body_height.max(1));
    let mut wall_min = wall_heights[0];
    let mut wall_max = wall_heights[0];
    let mut i = 1;
    while i < wall_heights.len() {
        wall_min = wall_min.min(wall_heights[i]);
        wall_max = wall_max.max(wall_heights[i]);
        i += 1;
    }
    body_max > wall_min && body_min < wall_max
}

fn circle_overlaps_wall_segment(
    cx: i32,
    cz: i32,
    radius: i32,
    sx: i32,
    sz: i32,
    sector_size: i32,
    direction: u8,
) -> bool {
    let Some((ax, az, bx, bz)) = wall_segment_xz(sx, sz, sector_size, direction) else {
        return false;
    };
    circle_overlaps_segment(cx, cz, radius, ax, az, bx, bz)
}

fn wall_segment_xz(
    sx: i32,
    sz: i32,
    sector_size: i32,
    direction: u8,
) -> Option<(i32, i32, i32, i32)> {
    let x0 = sx.saturating_mul(sector_size);
    let x1 = x0.saturating_add(sector_size);
    let z0 = sz.saturating_mul(sector_size);
    let z1 = z0.saturating_add(sector_size);
    match direction {
        DIR_NORTH => Some((x0, z0, x1, z0)),
        DIR_EAST => Some((x1, z0, x1, z1)),
        DIR_SOUTH => Some((x1, z1, x0, z1)),
        DIR_WEST => Some((x0, z1, x0, z0)),
        DIR_NORTH_WEST_SOUTH_EAST => Some((x0, z0, x1, z1)),
        DIR_NORTH_EAST_SOUTH_WEST => Some((x1, z0, x0, z1)),
        _ => None,
    }
}

fn circle_overlaps_segment(
    cx: i32,
    cz: i32,
    radius: i32,
    ax: i32,
    az: i32,
    bx: i32,
    bz: i32,
) -> bool {
    // Most walls in the small sector neighbourhood are nowhere near the body.
    // Reject them before the closest-point projection, whose Q12 divide is
    // comparatively expensive on the R3000.
    let radius = radius.max(0);
    if cx < ax.min(bx).saturating_sub(radius)
        || cx > ax.max(bx).saturating_add(radius)
        || cz < az.min(bz).saturating_sub(radius)
        || cz > az.max(bz).saturating_add(radius)
    {
        return false;
    }
    let vx = bx.saturating_sub(ax);
    let vz = bz.saturating_sub(az);
    let wx = cx.saturating_sub(ax);
    let wz = cz.saturating_sub(az);
    let len_sq = square_i32_saturating(vx).saturating_add(square_i32_saturating(vz));
    if len_sq <= 0 {
        return square_i32_saturating(cx.saturating_sub(ax))
            .saturating_add(square_i32_saturating(cz.saturating_sub(az)))
            <= square_i32_saturating(radius);
    }
    let dot = wx.saturating_mul(vx).saturating_add(wz.saturating_mul(vz));
    let t_q12 = div_q12_i32(dot, len_sq).clamp(0, Q12::SCALE);
    let t = Q12::from_raw(t_q12);
    let closest_x = ax.saturating_add(t.mul_i32(vx));
    let closest_z = az.saturating_add(t.mul_i32(vz));
    square_i32_saturating(cx.saturating_sub(closest_x))
        .saturating_add(square_i32_saturating(cz.saturating_sub(closest_z)))
        <= square_i32_saturating(radius)
}

fn body_hits_blocker(
    position: RoomPoint,
    radius: i32,
    height: i32,
    blockers: &[CharacterCollisionCylinder],
) -> bool {
    if radius <= 0 || height <= 0 {
        return false;
    }
    for blocker in blockers {
        if cylinder_overlaps(position, radius, height, *blocker) {
            return true;
        }
    }
    false
}

fn body_hits_aabb_blocker(
    position: RoomPoint,
    radius: i32,
    height: i32,
    blockers: &[CharacterCollisionAabb],
) -> bool {
    if radius <= 0 || height <= 0 {
        return false;
    }
    for blocker in blockers {
        if cylinder_overlaps_aabb(position, radius, height, *blocker) {
            return true;
        }
    }
    false
}

fn merge_collision_trace(best: &mut CollisionTrace, candidate: CollisionTrace) {
    let start_solid = best.start_solid || candidate.start_solid;
    let all_solid = best.all_solid || candidate.all_solid;
    if candidate.fraction_q12 < best.fraction_q12 {
        *best = candidate;
    }
    best.start_solid = start_solid;
    best.all_solid = all_solid;
}

/// Trace one upright moving body against one upright actor cylinder.
///
/// Actor blockers intentionally participate only in horizontal body sweeps
/// (and zero-length recovery probes). Downward support traces must continue to
/// resolve the BSP floor rather than treating another actor's head as terrain.
/// The closest-point interval is monotonic, so a 12-step Q0.12 binary search
/// finds the first deterministic contact without floating point or allocation.
fn trace_character_blocker(
    query: CollisionTraceQuery,
    blocker: CharacterCollisionCylinder,
) -> Option<CollisionTrace> {
    let CollisionTraceShape::Body { radius, height } = query.shape else {
        return None;
    };
    if radius <= 0
        || height <= 0
        || blocker.radius <= 0
        || blocker.height <= 0
        || query.start.y != query.end.y
    {
        return None;
    }
    let body_top = query.start.y.saturating_add(height);
    // The generic trace motor probes a raised path after any direct hit to
    // step over low WORLD risers. Actor bodies are never steppable in the grid
    // contract, so keep them blocking across that one bounded lift.
    let blocker_top = blocker
        .position
        .y
        .saturating_add(blocker.height)
        .saturating_add(STEP_UP_HEIGHT);
    if body_top <= blocker.position.y || blocker_top <= query.start.y {
        return None;
    }
    let combined_radius = radius.saturating_add(blocker.radius);
    if combined_radius <= 0 {
        return None;
    }
    let radius_sq = square_i32_saturating(combined_radius);
    let start_dx = query.start.x.saturating_sub(blocker.position.x);
    let start_dz = query.start.z.saturating_sub(blocker.position.z);
    let start_sq = square_i32_saturating(start_dx).saturating_add(square_i32_saturating(start_dz));
    let move_x = query.end.x.saturating_sub(query.start.x);
    let move_z = query.end.z.saturating_sub(query.start.z);
    if start_sq < radius_sq {
        let all_solid = blocker_overlap_at_fraction(query, blocker, combined_radius, Q12::SCALE);
        return Some(CollisionTrace {
            all_solid,
            start_solid: true,
            fraction_q12: 0,
            end: query.start,
            normal_q12: blocker_contact_normal(start_dx, start_dz, move_x, move_z),
            plane_distance: 0,
        });
    }
    if start_sq == radius_sq {
        let outward_dot = start_dx
            .saturating_mul(move_x)
            .saturating_add(start_dz.saturating_mul(move_z));
        if outward_dot >= 0 {
            return None;
        }
        return Some(CollisionTrace {
            all_solid: false,
            start_solid: false,
            fraction_q12: 0,
            end: query.start,
            normal_q12: blocker_contact_normal(start_dx, start_dz, move_x, move_z),
            plane_distance: 0,
        });
    }
    let length_sq = square_i32_saturating(move_x).saturating_add(square_i32_saturating(move_z));
    if length_sq <= 0 {
        return None;
    }

    // Project the blocker centre onto the swept centre segment. If the body
    // does not overlap at that closest Q0.12 point, the segment is clear.
    let to_center_x = blocker.position.x.saturating_sub(query.start.x);
    let to_center_z = blocker.position.z.saturating_sub(query.start.z);
    let projection = to_center_x
        .saturating_mul(move_x)
        .saturating_add(to_center_z.saturating_mul(move_z));
    let closest_q12 = div_q12_i32(projection, length_sq).clamp(0, Q12::SCALE);
    if closest_q12 <= 0
        || !blocker_overlap_at_fraction(query, blocker, combined_radius, closest_q12)
    {
        return None;
    }

    let mut clear_q12: i32 = 0;
    let mut contact_q12 = closest_q12;
    while clear_q12.saturating_add(1) < contact_q12 {
        let middle = clear_q12.saturating_add(contact_q12) / 2;
        if blocker_overlap_at_fraction(query, blocker, combined_radius, middle) {
            contact_q12 = middle;
        } else {
            clear_q12 = middle;
        }
    }
    // A contact exactly at the requested endpoint must still reject the
    // occupancy test; reserve 4096 for a genuinely unobstructed trace.
    let fraction_q12 = contact_q12.min(COLLISION_FRACTION_ONE_Q12 - 1);
    let end = trace_lerp_point(query.start, query.end, fraction_q12);
    let contact_dx = end.x.saturating_sub(blocker.position.x);
    let contact_dz = end.z.saturating_sub(blocker.position.z);
    Some(CollisionTrace {
        all_solid: false,
        start_solid: false,
        fraction_q12,
        end,
        normal_q12: blocker_contact_normal(contact_dx, contact_dz, move_x, move_z),
        plane_distance: 0,
    })
}

fn blocker_overlap_at_fraction(
    query: CollisionTraceQuery,
    blocker: CharacterCollisionCylinder,
    radius: i32,
    fraction_q12: i32,
) -> bool {
    let point = trace_lerp_point(query.start, query.end, fraction_q12);
    let dx = point.x.saturating_sub(blocker.position.x);
    let dz = point.z.saturating_sub(blocker.position.z);
    square_i32_saturating(dx).saturating_add(square_i32_saturating(dz))
        <= square_i32_saturating(radius)
}

/// Trace one upright moving body against one static prop AABB.
///
/// Like actor cylinders, prop blockers participate only in horizontal body
/// sweeps and zero-length recovery probes. They are not supporting BSP floors,
/// and the bounded step lift must not make a low prop silently steppable (the
/// grid backend has always treated these authored blockers as obstacles).
/// Candidate fractions cover every AABB side transition and every rounded
/// cylinder/AABB corner; the final entry is refined at Q0.12 precision.
fn trace_aabb_blocker(
    query: CollisionTraceQuery,
    blocker: CharacterCollisionAabb,
) -> Option<CollisionTrace> {
    let CollisionTraceShape::Body { radius, height } = query.shape else {
        return None;
    };
    if radius <= 0 || height <= 0 || query.start.y != query.end.y {
        return None;
    }
    let min = RoomPoint::new(
        blocker.min.x.min(blocker.max.x),
        blocker.min.y.min(blocker.max.y),
        blocker.min.z.min(blocker.max.z),
    );
    let max = RoomPoint::new(
        blocker.min.x.max(blocker.max.x),
        blocker.min.y.max(blocker.max.y),
        blocker.min.z.max(blocker.max.z),
    );
    if min.x == max.x || min.y == max.y || min.z == max.z {
        return None;
    }
    let body_top = query.start.y.saturating_add(height);
    let blocker_top = max.y.saturating_add(STEP_UP_HEIGHT);
    if body_top <= min.y || blocker_top <= query.start.y {
        return None;
    }
    let sweep_min_x = query.start.x.min(query.end.x).saturating_sub(radius);
    let sweep_max_x = query.start.x.max(query.end.x).saturating_add(radius);
    let sweep_min_z = query.start.z.min(query.end.z).saturating_sub(radius);
    let sweep_max_z = query.start.z.max(query.end.z).saturating_add(radius);
    if sweep_max_x < min.x || max.x < sweep_min_x || sweep_max_z < min.z || max.z < sweep_min_z {
        return None;
    }

    let radius_sq = square_i32_saturating(radius);
    let move_x = query.end.x.saturating_sub(query.start.x);
    let move_z = query.end.z.saturating_sub(query.start.z);
    let start_delta = aabb_contact_delta(query.start, min, max);
    let start_sq =
        square_i32_saturating(start_delta.0).saturating_add(square_i32_saturating(start_delta.1));
    if start_sq < radius_sq {
        return Some(CollisionTrace {
            all_solid: aabb_overlap_at_fraction(query, min, max, radius, Q12::SCALE),
            start_solid: true,
            fraction_q12: 0,
            end: query.start,
            normal_q12: aabb_contact_normal(query.start, min, max, move_x, move_z),
            plane_distance: 0,
        });
    }
    if start_sq == radius_sq {
        let outward_dot = start_delta
            .0
            .saturating_mul(move_x)
            .saturating_add(start_delta.1.saturating_mul(move_z));
        if outward_dot >= 0 {
            return None;
        }
        return Some(CollisionTrace {
            all_solid: false,
            start_solid: false,
            fraction_q12: 0,
            end: query.start,
            normal_q12: aabb_contact_normal(query.start, min, max, move_x, move_z),
            plane_distance: 0,
        });
    }

    let length_sq = square_i32_saturating(move_x).saturating_add(square_i32_saturating(move_z));
    if length_sq <= 0 {
        return None;
    }

    let mut closest_q12 = 0;
    let mut closest_sq = aabb_distance_sq_at_fraction_q12(query, min, max, 0);
    let mut consider = |candidate: i32| {
        // Q12 interpolation truncation can place a mathematical boundary on
        // either adjacent discrete fraction. Inspect both neighbors as part of
        // the candidate without scanning the whole segment.
        for offset in -1i32..=1 {
            let fraction = candidate.saturating_add(offset).clamp(0, Q12::SCALE);
            let distance_sq = aabb_distance_sq_at_fraction_q12(query, min, max, fraction);
            if distance_sq < closest_sq || (distance_sq == closest_sq && fraction < closest_q12) {
                closest_sq = distance_sq;
                closest_q12 = fraction;
            }
        }
    };
    consider(Q12::SCALE);
    if move_x != 0 {
        consider(div_q12_i32(min.x.saturating_sub(query.start.x), move_x));
        consider(div_q12_i32(max.x.saturating_sub(query.start.x), move_x));
    }
    if move_z != 0 {
        consider(div_q12_i32(min.z.saturating_sub(query.start.z), move_z));
        consider(div_q12_i32(max.z.saturating_sub(query.start.z), move_z));
    }
    for (corner_x, corner_z) in [
        (min.x, min.z),
        (min.x, max.z),
        (max.x, min.z),
        (max.x, max.z),
    ] {
        let to_corner_x = corner_x.saturating_sub(query.start.x);
        let to_corner_z = corner_z.saturating_sub(query.start.z);
        let projection = to_corner_x
            .saturating_mul(move_x)
            .saturating_add(to_corner_z.saturating_mul(move_z));
        consider(div_q12_i32(projection, length_sq));
    }
    let radius_q8 = radius.saturating_mul(256);
    if closest_sq > square_i32_saturating(radius_q8)
        || !aabb_overlap_at_fraction(query, min, max, radius, closest_q12)
    {
        return None;
    }

    let mut clear_q12 = 0i32;
    let mut contact_q12 = closest_q12;
    while clear_q12.saturating_add(1) < contact_q12 {
        let middle = clear_q12.saturating_add(contact_q12) / 2;
        if aabb_overlap_at_fraction(query, min, max, radius, middle) {
            contact_q12 = middle;
        } else {
            clear_q12 = middle;
        }
    }
    // Q24.8 keeps the hot path 32-bit on PS1, but its per-axis quantisation can
    // move a rounded-corner entry by a handful of Q0.12 fractions. Search the
    // bounded neighborhood behind the binary result so the earliest discrete
    // overlapping fraction remains authoritative.
    let refine_start = contact_q12.saturating_sub(16);
    for candidate in refine_start..contact_q12 {
        if aabb_overlap_at_fraction(query, min, max, radius, candidate) {
            contact_q12 = candidate;
            break;
        }
    }
    let fraction_q12 = contact_q12.min(COLLISION_FRACTION_ONE_Q12 - 1);
    let end = trace_lerp_point(query.start, query.end, fraction_q12);
    Some(CollisionTrace {
        all_solid: false,
        start_solid: false,
        fraction_q12,
        end,
        normal_q12: aabb_contact_normal(end, min, max, move_x, move_z),
        plane_distance: 0,
    })
}

fn aabb_overlap_at_fraction(
    query: CollisionTraceQuery,
    min: RoomPoint,
    max: RoomPoint,
    radius: i32,
    fraction_q12: i32,
) -> bool {
    let radius_q8 = radius.max(0).saturating_mul(256);
    aabb_distance_sq_at_fraction_q12(query, min, max, fraction_q12)
        <= square_i32_saturating(radius_q8)
}

fn aabb_distance_sq_at_fraction_q12(
    query: CollisionTraceQuery,
    min: RoomPoint,
    max: RoomPoint,
    fraction_q12: i32,
) -> i32 {
    // Keep the moving centre in Q24.8 for the overlap predicate. Rounding X
    // and Z to whole engine units independently can make a diagonal path's
    // distance non-convex (and hide a one-fraction corner crossing), while
    // Q20.12 squared would require costly 64-bit helpers on PS1. Q24.8 retains
    // 1/256-unit precision using the engine's normal saturating 32-bit math.
    let fraction_q12 = fraction_q12.clamp(0, Q12::SCALE);
    let point_x_q8 = query
        .start
        .x
        .saturating_mul(256)
        .saturating_add(
            query
                .end
                .x
                .saturating_sub(query.start.x)
                .saturating_mul(fraction_q12)
                >> 4,
        );
    let point_z_q8 = query
        .start
        .z
        .saturating_mul(256)
        .saturating_add(
            query
                .end
                .z
                .saturating_sub(query.start.z)
                .saturating_mul(fraction_q12)
                >> 4,
        );
    let min_x_q8 = min.x.saturating_mul(256);
    let max_x_q8 = max.x.saturating_mul(256);
    let min_z_q8 = min.z.saturating_mul(256);
    let max_z_q8 = max.z.saturating_mul(256);
    let dx = point_x_q8.saturating_sub(point_x_q8.clamp(min_x_q8, max_x_q8));
    let dz = point_z_q8.saturating_sub(point_z_q8.clamp(min_z_q8, max_z_q8));
    square_i32_saturating(dx).saturating_add(square_i32_saturating(dz))
}

fn aabb_contact_delta(point: RoomPoint, min: RoomPoint, max: RoomPoint) -> (i32, i32) {
    let closest_x = point.x.clamp(min.x, max.x);
    let closest_z = point.z.clamp(min.z, max.z);
    (
        point.x.saturating_sub(closest_x),
        point.z.saturating_sub(closest_z),
    )
}

fn aabb_contact_normal(
    point: RoomPoint,
    min: RoomPoint,
    max: RoomPoint,
    move_x: i32,
    move_z: i32,
) -> [i16; 3] {
    let delta = aabb_contact_delta(point, min, max);
    if delta != (0, 0) {
        return blocker_contact_normal(delta.0, delta.1, move_x, move_z);
    }
    // A recovery probe can begin inside the box. Pick the nearest horizontal
    // face deterministically; direction breaks only exact distance ties.
    let faces = [
        (point.x.saturating_sub(min.x), [-4096, 0, 0]),
        (max.x.saturating_sub(point.x), [4096, 0, 0]),
        (point.z.saturating_sub(min.z), [0, 0, -4096]),
        (max.z.saturating_sub(point.z), [0, 0, 4096]),
    ];
    let mut best = faces[0];
    for face in faces.into_iter().skip(1) {
        if face.0 < best.0 {
            best = face;
        }
    }
    if faces.iter().filter(|face| face.0 == best.0).count() > 1 {
        return blocker_contact_normal(0, 0, move_x, move_z);
    }
    best.1
}

fn trace_lerp_point(start: RoomPoint, end: RoomPoint, fraction_q12: i32) -> RoomPoint {
    let fraction = Q12::from_raw(fraction_q12.clamp(0, Q12::SCALE));
    RoomPoint::new(
        start
            .x
            .saturating_add(fraction.mul_i32(end.x.saturating_sub(start.x))),
        start
            .y
            .saturating_add(fraction.mul_i32(end.y.saturating_sub(start.y))),
        start
            .z
            .saturating_add(fraction.mul_i32(end.z.saturating_sub(start.z))),
    )
}

fn blocker_contact_normal(dx: i32, dz: i32, move_x: i32, move_z: i32) -> [i16; 3] {
    let length = isqrt_i32(square_i32_saturating(dx).saturating_add(square_i32_saturating(dz)));
    if length > 0 {
        let nx = dx
            .saturating_mul(Q12::SCALE)
            .checked_div(length)
            .unwrap_or(0)
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        let nz = dz
            .saturating_mul(Q12::SCALE)
            .checked_div(length)
            .unwrap_or(0)
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        return [nx, 0, nz];
    }
    if move_x.saturating_abs() >= move_z.saturating_abs() {
        [if move_x >= 0 { -4096 } else { 4096 }, 0, 0]
    } else {
        [0, 0, if move_z >= 0 { -4096 } else { 4096 }]
    }
}

fn cylinder_overlaps(
    position: RoomPoint,
    radius: i32,
    height: i32,
    blocker: CharacterCollisionCylinder,
) -> bool {
    let other_radius = blocker.radius.max(0);
    let other_height = blocker.height.max(0);
    if other_radius == 0 || other_height == 0 {
        return false;
    }
    let top = position.y.saturating_add(height.max(1));
    let other_top = blocker.position.y.saturating_add(other_height);
    if top <= blocker.position.y || other_top <= position.y {
        return false;
    }
    let radius_sum = radius.max(0).saturating_add(other_radius);
    if radius_sum <= 0 {
        return false;
    }
    let dx = position.x.saturating_sub(blocker.position.x);
    let dz = position.z.saturating_sub(blocker.position.z);
    square_i32_saturating(dx).saturating_add(square_i32_saturating(dz))
        <= square_i32_saturating(radius_sum)
}

fn cylinder_overlaps_aabb(
    position: RoomPoint,
    radius: i32,
    height: i32,
    blocker: CharacterCollisionAabb,
) -> bool {
    let min_x = blocker.min.x.min(blocker.max.x);
    let max_x = blocker.min.x.max(blocker.max.x);
    let min_y = blocker.min.y.min(blocker.max.y);
    let max_y = blocker.min.y.max(blocker.max.y);
    let min_z = blocker.min.z.min(blocker.max.z);
    let max_z = blocker.min.z.max(blocker.max.z);
    if min_x == max_x || min_y == max_y || min_z == max_z {
        return false;
    }
    let top = position.y.saturating_add(height.max(1));
    if top <= min_y || max_y <= position.y {
        return false;
    }
    let closest_x = position.x.clamp(min_x, max_x);
    let closest_z = position.z.clamp(min_z, max_z);
    let dx = position.x.saturating_sub(closest_x);
    let dz = position.z.saturating_sub(closest_z);
    square_i32_saturating(dx).saturating_add(square_i32_saturating(dz))
        <= square_i32_saturating(radius.max(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeRoom;

    struct FlatTraceProvider {
        calls: u8,
        fail_on_call: Option<u8>,
    }

    impl FlatTraceProvider {
        const fn new(fail_on_call: Option<u8>) -> Self {
            Self {
                calls: 0,
                fail_on_call,
            }
        }
    }

    impl CollisionTraceProvider for FlatTraceProvider {
        fn trace_into(
            &mut self,
            query: CollisionTraceQuery,
            output: &mut crate::CollisionTrace,
        ) -> bool {
            self.calls = self.calls.saturating_add(1);
            if self.fail_on_call == Some(self.calls) {
                return false;
            }
            let mut trace = crate::CollisionTrace::unobstructed(query.end);
            if query.start.y > 0 && query.end.y <= 0 {
                let distance = query.start.y.saturating_sub(query.end.y).max(1);
                trace.fraction_q12 =
                    query.start.y.saturating_mul(COLLISION_FRACTION_ONE_Q12) / distance;
                trace.end = RoomPoint::new(query.end.x, 0, query.end.z);
                trace.normal_q12 = [0, COLLISION_FRACTION_ONE_Q12 as i16, 0];
            }
            *output = trace;
            true
        }
    }

    fn config() -> CharacterMotorConfig {
        CharacterMotorConfig::character(64, 32, 64, Angle::from_q12(16))
    }

    #[test]
    fn trace_provider_advances_motor_on_flat_floor() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut provider = FlatTraceProvider::new(None);
        let frame = motor
            .update_vblanks_with_trace_provider(
                &mut provider,
                CharacterMotorInput {
                    walk: 1,
                    ..CharacterMotorInput::default()
                },
                config(),
                1,
            )
            .expect("trace update");
        assert!(frame.moved);
        assert!(!frame.blocked);
        assert_eq!(frame.position, RoomPoint::new(0, 0, 32));
        assert_eq!(provider.calls, 3);
    }

    #[test]
    fn trace_provider_failure_rolls_back_complete_motor_state() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let before = motor;
        let mut provider = FlatTraceProvider::new(Some(2));
        let result = motor.update_vblanks_with_trace_provider(
            &mut provider,
            CharacterMotorInput {
                walk: 1,
                turn: 1,
                ..CharacterMotorInput::default()
            },
            config(),
            1,
        );
        assert_eq!(result, Err(CollisionQueryError));
        assert_eq!(motor, before);
    }

    struct FixedTraceProvider {
        trace: CollisionTrace,
        fail_once: bool,
    }

    impl CollisionTraceProvider for FixedTraceProvider {
        fn trace_into(&mut self, _query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool {
            if self.fail_once {
                self.fail_once = false;
                return false;
            }
            *output = self.trace;
            true
        }
    }

    #[test]
    fn blocker_trace_hits_swept_actor_before_endpoint() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 0, 0), RoomPoint::new(20, 0, 0), 2, 8);
        let blockers = [CharacterCollisionCylinder::new(
            RoomPoint::new(10, 0, 0),
            2,
            8,
        )];
        let mut world = FixedTraceProvider {
            trace: CollisionTrace::unobstructed(query.end),
            fail_once: false,
        };
        let mut provider = CharacterBlockerTraceProvider::new(&mut world, &blockers);
        let trace = trace_collision(&mut provider, query).expect("compound trace");
        assert!(trace.hit());
        assert!(!trace.start_solid);
        assert!(trace.end.x >= 5 && trace.end.x <= 6, "{trace:?}");
        assert_eq!(trace.normal_q12[1], 0);
        assert!(trace.normal_q12[0] < 0);
    }

    #[test]
    fn blocker_trace_allows_motion_away_from_exact_tangent() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(6, 0, 0), RoomPoint::new(-8, 0, 0), 2, 8);
        let blockers = [CharacterCollisionCylinder::new(
            RoomPoint::new(10, 0, 0),
            2,
            8,
        )];
        let clear = CollisionTrace::unobstructed(query.end);
        let mut world = FixedTraceProvider {
            trace: clear,
            fail_once: false,
        };
        let mut provider = CharacterBlockerTraceProvider::new(&mut world, &blockers);
        assert_eq!(trace_collision(&mut provider, query), Ok(clear));
    }

    #[test]
    fn aabb_trace_hits_swept_prop_before_endpoint() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 0, 0), RoomPoint::new(40, 0, 0), 2, 8);
        let aabbs = [CharacterCollisionAabb::new(
            RoomPoint::new(10, 0, -4),
            RoomPoint::new(14, 8, 4),
        )];
        let mut world = FixedTraceProvider {
            trace: CollisionTrace::unobstructed(query.end),
            fail_once: false,
        };
        let mut provider = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &aabbs);
        let trace = trace_collision(&mut provider, query).expect("compound trace");
        assert!(trace.hit());
        assert!(!trace.start_solid);
        assert!((7..=8).contains(&trace.end.x), "{trace:?}");
        assert_eq!(trace.normal_q12, [-4096, 0, 0]);
    }

    #[test]
    fn aabb_trace_does_not_square_off_rounded_body_corner() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 0, 6), RoomPoint::new(7, 0, 6), 4, 8);
        let aabbs = [CharacterCollisionAabb::new(
            RoomPoint::new(10, 0, 10),
            RoomPoint::new(20, 8, 20),
        )];
        let clear = CollisionTrace::unobstructed(query.end);
        let mut world = FixedTraceProvider {
            trace: clear,
            fail_once: false,
        };
        let mut provider = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &aabbs);
        assert_eq!(trace_collision(&mut provider, query), Ok(clear));
    }

    #[test]
    fn aabb_trace_allows_motion_away_from_exact_tangent() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(8, 0, 0), RoomPoint::new(-20, 0, 0), 2, 8);
        let aabbs = [CharacterCollisionAabb::new(
            RoomPoint::new(10, 0, -4),
            RoomPoint::new(14, 8, 4),
        )];
        let clear = CollisionTrace::unobstructed(query.end);
        let mut world = FixedTraceProvider {
            trace: clear,
            fail_once: false,
        };
        let mut provider = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &aabbs);
        assert_eq!(trace_collision(&mut provider, query), Ok(clear));
    }

    #[test]
    fn aabb_trace_matches_exhaustive_q12_entry_for_crossing_paths() {
        let min = RoomPoint::new(-4, 0, -3);
        let max = RoomPoint::new(5, 8, 6);
        let blocker = CharacterCollisionAabb::new(min, max);
        let coordinates = [-16, -9, 0, 9, 16];
        for radius in [1, 3, 7] {
            let radius_sq = square_i32_saturating(radius);
            for start_x in coordinates {
                for start_z in coordinates {
                    let start = RoomPoint::new(start_x, 0, start_z);
                    let start_delta = aabb_contact_delta(start, min, max);
                    let start_sq = square_i32_saturating(start_delta.0)
                        .saturating_add(square_i32_saturating(start_delta.1));
                    if start_sq <= radius_sq {
                        continue;
                    }
                    for end_x in coordinates {
                        for end_z in coordinates {
                            let end = RoomPoint::new(end_x, 0, end_z);
                            if end == start {
                                continue;
                            }
                            let query = CollisionTraceQuery::body(start, end, radius, 8);
                            let expected = (0..=Q12::SCALE).find(|&fraction| {
                                aabb_overlap_at_fraction(query, min, max, radius, fraction)
                            });
                            let actual = trace_aabb_blocker(query, blocker);
                            assert_eq!(
                                actual.is_some(),
                                expected.is_some(),
                                "radius={radius} start={start:?} end={end:?} actual={actual:?} expected={expected:?}"
                            );
                            if let (Some(actual), Some(expected)) = (actual, expected) {
                                assert_eq!(
                                    actual.fraction_q12,
                                    expected.min(COLLISION_FRACTION_ONE_Q12 - 1),
                                    "radius={radius} start={start:?} end={end:?} actual={actual:?} expected={expected}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn world_hit_wins_exact_fraction_tie_with_dynamic_blocker() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 0, 0), RoomPoint::new(20, 0, 0), 2, 8);
        let blocker = CharacterCollisionCylinder::new(RoomPoint::new(10, 0, 0), 2, 8);
        let dynamic = trace_character_blocker(query, blocker).expect("dynamic candidate");
        let world_normal = [0, 0, -4096];
        let mut world = FixedTraceProvider {
            trace: CollisionTrace {
                normal_q12: world_normal,
                ..dynamic
            },
            fail_once: false,
        };
        let blockers = [blocker];
        let mut provider = CharacterBlockerTraceProvider::new(&mut world, &blockers);
        let trace = trace_collision(&mut provider, query).expect("compound trace");
        assert_eq!(trace.fraction_q12, dynamic.fraction_q12);
        assert_eq!(trace.normal_q12, world_normal);
    }

    #[test]
    fn world_hit_wins_exact_fraction_tie_with_aabb_blocker() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 0, 0), RoomPoint::new(20, 0, 0), 2, 8);
        let blocker =
            CharacterCollisionAabb::new(RoomPoint::new(10, 0, -4), RoomPoint::new(14, 8, 4));
        let prop = trace_aabb_blocker(query, blocker).expect("prop candidate");
        let world_normal = [0, 0, -4096];
        let mut world = FixedTraceProvider {
            trace: CollisionTrace {
                normal_q12: world_normal,
                ..prop
            },
            fail_once: false,
        };
        let aabbs = [blocker];
        let mut provider = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &aabbs);
        let trace = trace_collision(&mut provider, query).expect("compound trace");
        assert_eq!(trace.fraction_q12, prop.fraction_q12);
        assert_eq!(trace.normal_q12, world_normal);
    }

    #[test]
    fn blocker_layer_preserves_failed_output_and_reuses_immediately() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 0, 0), RoomPoint::new(20, 0, 0), 2, 8);
        let blockers = [CharacterCollisionCylinder::new(
            RoomPoint::new(10, 0, 0),
            2,
            8,
        )];
        let aabbs = [CharacterCollisionAabb::new(
            RoomPoint::new(12, 0, -4),
            RoomPoint::new(16, 8, 4),
        )];
        let mut world = FixedTraceProvider {
            trace: CollisionTrace::unobstructed(query.end),
            fail_once: true,
        };
        let sentinel = CollisionTrace {
            all_solid: true,
            start_solid: true,
            fraction_q12: 17,
            end: RoomPoint::new(1, 2, 3),
            normal_q12: [4, 5, 6],
            plane_distance: 7,
        };
        let mut output = sentinel;
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &blockers, &aabbs);
        assert!(!provider.trace_into(query, &mut output));
        assert_eq!(output, sentinel);
        assert!(provider.trace_into(query, &mut output));
        assert!(output.hit());
        assert_ne!(output, sentinel);
    }

    #[test]
    fn malformed_or_overflow_prop_state_fails_without_touching_output() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 0, 0), RoomPoint::new(20, 0, 0), 2, 8);
        let sentinel = CollisionTrace {
            all_solid: true,
            start_solid: true,
            fraction_q12: 17,
            end: RoomPoint::new(1, 2, 3),
            normal_q12: [4, 5, 6],
            plane_distance: 7,
        };
        let mut world = FixedTraceProvider {
            trace: CollisionTrace::unobstructed(query.end),
            fail_once: false,
        };
        let malformed = [CharacterCollisionAabb::new(
            RoomPoint::new(10, 8, -4),
            RoomPoint::new(14, 0, 4),
        )];
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &malformed);
        let mut output = sentinel;
        assert!(!provider.trace_into(query, &mut output));
        assert_eq!(output, sentinel);

        let valid =
            CharacterCollisionAabb::new(RoomPoint::new(10, 0, -4), RoomPoint::new(14, 8, 4));
        let overflow = [valid; psx_level::MAX_STATIC_PROP_AABB_BLOCKERS + 1];
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &overflow);
        assert!(!provider.trace_into(query, &mut output));
        assert_eq!(output, sentinel);
    }

    #[test]
    fn actor_wins_an_exact_fraction_tie_before_prop() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 0, 0), RoomPoint::new(20, 0, 0), 2, 8);
        let actor = CharacterCollisionCylinder::new(RoomPoint::new(10, 0, 4), 2, 8);
        let prop = CharacterCollisionAabb::new(RoomPoint::new(12, 0, -8), RoomPoint::new(16, 8, 8));
        let actor_trace = trace_character_blocker(query, actor).expect("actor candidate");
        let prop_trace = trace_aabb_blocker(query, prop).expect("prop candidate");
        assert_eq!(actor_trace.fraction_q12, prop_trace.fraction_q12);
        assert_ne!(actor_trace.normal_q12, prop_trace.normal_q12);
        let mut world = FixedTraceProvider {
            trace: CollisionTrace::unobstructed(query.end),
            fail_once: false,
        };
        let actors = [actor];
        let props = [prop];
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &actors, &props);
        let trace = trace_collision(&mut provider, query).expect("compound trace");
        assert_eq!(trace.fraction_q12, actor_trace.fraction_q12);
        assert_eq!(trace.normal_q12, actor_trace.normal_q12);
    }

    #[test]
    fn multi_room_origins_preserve_prop_contact_coordinates() {
        for origin in [
            RoomPoint::new(240_000, 12_000, -180_000),
            RoomPoint::new(-220_000, -8_000, 190_000),
        ] {
            let query = CollisionTraceQuery::body(
                origin,
                RoomPoint::new(origin.x + 40, origin.y, origin.z),
                2,
                8,
            );
            let prop = CharacterCollisionAabb::new(
                RoomPoint::new(origin.x + 10, origin.y, origin.z - 4),
                RoomPoint::new(origin.x + 14, origin.y + 8, origin.z + 4),
            );
            let mut world = FixedTraceProvider {
                trace: CollisionTrace::unobstructed(query.end),
                fail_once: false,
            };
            let props = [prop];
            let mut provider =
                CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &props);
            let trace = trace_collision(&mut provider, query).expect("large-origin trace");
            assert!(trace.hit());
            assert!((origin.x + 7..=origin.x + 8).contains(&trace.end.x));
            assert_eq!(trace.end.z, origin.z);
        }
    }

    #[test]
    fn empty_prop_layer_is_one_bounded_wrapped_trace() {
        struct CountingProvider {
            calls: u8,
        }
        impl CollisionTraceProvider for CountingProvider {
            fn trace_into(
                &mut self,
                query: CollisionTraceQuery,
                output: &mut CollisionTrace,
            ) -> bool {
                self.calls = self.calls.saturating_add(1);
                *output = CollisionTrace::unobstructed(query.end);
                true
            }
        }

        let query = CollisionTraceQuery::body(RoomPoint::ZERO, RoomPoint::new(20, 0, 0), 2, 8);
        let mut world = CountingProvider { calls: 0 };
        let mut provider = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &[]);
        assert_eq!(
            trace_collision(&mut provider, query),
            Ok(CollisionTrace::unobstructed(query.end))
        );
        drop(provider);
        assert_eq!(world.calls, 1);
    }

    #[test]
    fn downward_floor_probe_ignores_actor_heads() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 20, 0), RoomPoint::new(0, -20, 0), 2, 8);
        let blockers = [CharacterCollisionCylinder::new(
            RoomPoint::new(0, 0, 0),
            4,
            10,
        )];
        let clear = CollisionTrace::unobstructed(query.end);
        let mut world = FixedTraceProvider {
            trace: clear,
            fail_once: false,
        };
        let mut provider = CharacterBlockerTraceProvider::new(&mut world, &blockers);
        assert_eq!(trace_collision(&mut provider, query), Ok(clear));
    }

    #[test]
    fn downward_floor_probe_ignores_prop_tops() {
        let query =
            CollisionTraceQuery::body(RoomPoint::new(0, 20, 0), RoomPoint::new(0, -20, 0), 2, 8);
        let aabbs = [CharacterCollisionAabb::new(
            RoomPoint::new(-4, 0, -4),
            RoomPoint::new(4, 10, 4),
        )];
        let clear = CollisionTrace::unobstructed(query.end);
        let mut world = FixedTraceProvider {
            trace: clear,
            fail_once: false,
        };
        let mut provider = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &aabbs);
        assert_eq!(trace_collision(&mut provider, query), Ok(clear));
    }

    #[test]
    fn trace_body_step_respects_aabb_blocker() {
        let aabbs = [CharacterCollisionAabb::new(
            RoomPoint::new(8, 0, -4),
            RoomPoint::new(14, 8, 4),
        )];
        let mut world = FlatTraceProvider::new(None);
        let mut provider = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &aabbs);
        let step =
            commit_body_step_with_trace_provider(&mut provider, RoomPoint::ZERO, 20, 0, 2, 8)
                .expect("trace body step");
        assert_eq!(step.position, RoomPoint::ZERO);
        assert!(!step.moved);
        assert!(step.blocked);
    }

    #[test]
    fn trace_body_step_uses_full_then_x_then_z_blocker_order() {
        let blockers = [CharacterCollisionCylinder::new(
            RoomPoint::new(10, 0, 10),
            2,
            8,
        )];
        let mut world = FlatTraceProvider::new(None);
        let mut provider = CharacterBlockerTraceProvider::new(&mut world, &blockers);
        let step =
            commit_body_step_with_trace_provider(&mut provider, RoomPoint::ZERO, 20, 20, 2, 8)
                .expect("trace body step");
        assert_eq!(step.position, RoomPoint::new(20, 0, 0));
        assert!(step.moved);
        assert!(step.blocked);
    }

    #[test]
    fn low_riser_is_steppable_not_blocking() {
        // Feet on the lower floor at y=0, body 768 tall. A short riser
        // (top 320, a demo-scale step) overlaps the body but is within a
        // step of the feet, so it must NOT block: the character steps up.
        let step = [0, 0, 320, 320];
        assert!(vertical_ranges_overlap(0, 768, step), "step overlaps body");
        assert!(
            !wall_blocks_body(0, 768, step),
            "a low riser within STEP_UP_HEIGHT must be steppable"
        );
    }

    #[test]
    fn full_wall_still_blocks() {
        // A real wall (top 1792, a full sector) rises far above the feet
        // and must block.
        let wall = [0, 0, 1792, 1792];
        assert!(
            wall_blocks_body(0, 768, wall),
            "a full-height wall must block"
        );
    }

    #[test]
    fn step_at_threshold_boundary() {
        // Exactly STEP_UP_HEIGHT above the feet is still steppable; one
        // unit higher blocks. Guards the off-by-one at the boundary.
        let at = [0, 0, STEP_UP_HEIGHT, STEP_UP_HEIGHT];
        let over = [0, 0, STEP_UP_HEIGHT + 1, STEP_UP_HEIGHT + 1];
        assert!(
            !wall_blocks_body(0, 768, at),
            "top == feet+STEP_UP steppable"
        );
        assert!(wall_blocks_body(0, 768, over), "one unit higher blocks");
    }

    #[test]
    fn wall_below_feet_does_not_block() {
        // A wall entirely below the feet (e.g. seen from an upper floor)
        // doesn't overlap the body and never blocks.
        let below = [-1024, -1024, -512, -512];
        assert!(!wall_blocks_body(0, 768, below));
    }

    fn world_with_internal_south_wall() -> [u8; 184] {
        const ASSET_HEADER: usize = 12;
        const WORLD_HEADER: usize = 20;
        const SECTOR_RECORD: usize = 60;
        const WALL_RECORD: usize = 32;
        const SECTOR0: usize = ASSET_HEADER + WORLD_HEADER;
        const SECTOR1: usize = SECTOR0 + SECTOR_RECORD;
        const WALL0: usize = SECTOR1 + SECTOR_RECORD;
        let payload_len = (WORLD_HEADER + SECTOR_RECORD * 2 + WALL_RECORD) as u32;
        let mut buf = [0u8; 184];
        buf[0..4].copy_from_slice(b"PSXW");
        buf[4..6].copy_from_slice(&3u16.to_le_bytes());
        buf[8..12].copy_from_slice(&payload_len.to_le_bytes());
        buf[12..14].copy_from_slice(&1u16.to_le_bytes());
        buf[14..16].copy_from_slice(&2u16.to_le_bytes());
        buf[16..20].copy_from_slice(&1024i32.to_le_bytes());
        buf[20..22].copy_from_slice(&2u16.to_le_bytes());
        buf[22..24].copy_from_slice(&1u16.to_le_bytes());
        buf[24..26].copy_from_slice(&1u16.to_le_bytes());

        buf[SECTOR0] = 1 | 4;
        buf[SECTOR0 + 8..SECTOR0 + 10].copy_from_slice(&0u16.to_le_bytes());
        buf[SECTOR0 + 10..SECTOR0 + 12].copy_from_slice(&1u16.to_le_bytes());
        buf[SECTOR1] = 1 | 4;
        buf[SECTOR1 + 8..SECTOR1 + 10].copy_from_slice(&1u16.to_le_bytes());

        buf[WALL0] = DIR_SOUTH;
        buf[WALL0 + 1] = 1;
        buf[WALL0 + 8..WALL0 + 12].copy_from_slice(&0i32.to_le_bytes());
        buf[WALL0 + 12..WALL0 + 16].copy_from_slice(&0i32.to_le_bytes());
        buf[WALL0 + 16..WALL0 + 20].copy_from_slice(&1024i32.to_le_bytes());
        buf[WALL0 + 20..WALL0 + 24].copy_from_slice(&1024i32.to_le_bytes());
        buf
    }

    fn flat_floor_world() -> [u8; 92] {
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
        buf
    }

    fn sparse_two_sector_world() -> [u8; 152] {
        const ASSET_HEADER: usize = 12;
        const WORLD_HEADER: usize = 20;
        const SECTOR_RECORD: usize = 60;
        const SECTOR0: usize = ASSET_HEADER + WORLD_HEADER;
        let payload_len = (WORLD_HEADER + SECTOR_RECORD * 2) as u32;
        let mut buf = [0u8; 152];
        buf[0..4].copy_from_slice(b"PSXW");
        buf[4..6].copy_from_slice(&3u16.to_le_bytes());
        buf[8..12].copy_from_slice(&payload_len.to_le_bytes());
        buf[12..14].copy_from_slice(&2u16.to_le_bytes());
        buf[14..16].copy_from_slice(&1u16.to_le_bytes());
        buf[16..20].copy_from_slice(&1024i32.to_le_bytes());
        buf[20..22].copy_from_slice(&2u16.to_le_bytes());
        buf[22..24].copy_from_slice(&1u16.to_le_bytes());

        buf[SECTOR0] = 1 | 4;
        buf[SECTOR0 + 4..SECTOR0 + 6].copy_from_slice(&0u16.to_le_bytes());
        buf
    }

    #[test]
    fn forward_input_moves_along_yaw() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                walk: 1,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.position, RoomPoint::new(0, 0, 32));
        assert_eq!(frame.anim, CharacterMotorAnim::Walk);
        assert!(frame.moved);
    }

    #[test]
    fn turn_input_wraps_yaw() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                turn: -1,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.yaw, Angle::ZERO.add_signed_q12(-16));
    }

    #[test]
    fn analog_vector_moves_without_tank_turning() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                move_x: Q12::ONE,
                move_z: Q12::ZERO,
                walk: 1,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.position, RoomPoint::new(32, 0, 0));
        assert_eq!(frame.yaw, Angle::QUARTER);
        assert_eq!(frame.anim, CharacterMotorAnim::Walk);
        assert!(frame.moved);
    }

    #[test]
    fn locked_evade_slides_directionally_and_preserves_facing() {
        let cases = [
            (Q12::ZERO, Q12::ONE, CharacterMotorAnim::Roll),
            (Q12::ZERO, Q12::NEG_ONE, CharacterMotorAnim::Quickstep),
            (Q12::NEG_ONE, Q12::ZERO, CharacterMotorAnim::DashLeft),
            (Q12::ONE, Q12::ZERO, CharacterMotorAnim::DashRight),
        ];
        for (move_x, move_z, expected) in cases {
            let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
            let frame = motor.update(
                None,
                CharacterMotorInput {
                    move_x,
                    move_z,
                    evade: true,
                    facing_yaw: Some(Angle::ZERO),
                    ..CharacterMotorInput::default()
                },
                config(),
            );
            // The body keeps facing the target; only the slide direction
            // and the reported clip intent change.
            assert_eq!(frame.yaw, Angle::ZERO);
            assert_eq!(frame.anim, expected);
            assert_eq!(frame.action, CharacterMotorAction::Roll);
            assert!(frame.moved);
        }
    }

    #[test]
    fn free_evade_still_turns_into_the_roll() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                move_x: Q12::ONE,
                move_z: Q12::ZERO,
                evade: true,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.yaw, Angle::QUARTER);
        assert_eq!(frame.anim, CharacterMotorAnim::Roll);
        assert_eq!(frame.action, CharacterMotorAction::Roll);
    }

    #[test]
    fn locked_movement_preserves_facing_and_reports_directional_intent() {
        let cases = [
            (Q12::ZERO, Q12::ONE, CharacterMotorAnim::Walk),
            (Q12::ZERO, Q12::NEG_ONE, CharacterMotorAnim::WalkBackward),
            (Q12::NEG_ONE, Q12::ZERO, CharacterMotorAnim::StrafeLeft),
            (Q12::ONE, Q12::ZERO, CharacterMotorAnim::StrafeRight),
        ];
        for (move_x, move_z, expected) in cases {
            let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
            let frame = motor.update(
                None,
                CharacterMotorInput {
                    move_x,
                    move_z,
                    facing_yaw: Some(Angle::ZERO),
                    ..CharacterMotorInput::default()
                },
                config(),
            );
            assert_eq!(frame.yaw, Angle::ZERO);
            assert_eq!(frame.anim, expected);
            assert!(!frame.sprinting);
        }
    }

    #[test]
    fn locked_sprint_only_applies_toward_the_target() {
        // Dark Souls' contract: locked on, you charge at full speed toward the
        // target or fast-walk sideways relative to it. Sprint is suppressed for
        // lateral and backward movement, so those keep walk speed AND the
        // walk-direction clip, and no run-strafe animation is ever needed.
        let directions = [
            (Q12::ZERO, Q12::ONE),
            (Q12::ZERO, Q12::NEG_ONE),
            (Q12::NEG_ONE, Q12::ZERO),
            (Q12::ONE, Q12::ZERO),
            (Q12::ONE, Q12::ONE),
            (Q12::NEG_ONE, Q12::ONE),
            (Q12::ONE, Q12::NEG_ONE),
            (Q12::NEG_ONE, Q12::NEG_ONE),
        ];
        for (move_x, move_z) in directions {
            let input = |sprint| CharacterMotorInput {
                move_x,
                move_z,
                facing_yaw: Some(Angle::ZERO),
                sprint,
                ..CharacterMotorInput::default()
            };
            let walked = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO)
                .update(None, input(false), config());
            let sprinted = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO)
                .update(None, input(true), config());

            // Facing is held on the target regardless of speed or direction.
            assert_eq!(walked.yaw, Angle::ZERO);
            assert_eq!(sprinted.yaw, Angle::ZERO);

            if walked.anim == CharacterMotorAnim::Walk {
                // Toward the target: sprint engages and swaps to the run clip.
                assert!(sprinted.sprinting, "forward should sprint");
                assert_eq!(sprinted.anim, CharacterMotorAnim::Run);
            } else {
                // Anything else: sprint is refused, clip and speed unchanged.
                assert!(
                    !sprinted.sprinting,
                    "lateral/backward must not sprint under lock ({move_x:?}, {move_z:?})"
                );
                assert_eq!(sprinted.anim, walked.anim);
                assert_eq!(sprinted.position, walked.position);
            }
        }
    }

    #[test]
    fn locked_lateral_stick_refuses_sprint_and_strafes_at_walk_speed() {
        // Camera-relative forward can be lateral to the target for a few frames
        // right after lock-on. The character strafes at walk speed facing the
        // target rather than turning away into a run.
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                walk: 1,
                move_x: Q12::ONE,
                facing_yaw: Some(Angle::ZERO),
                sprint: true,
                ..CharacterMotorInput::default()
            },
            config(),
        );

        assert_eq!(frame.yaw, Angle::ZERO, "facing stays on the target");
        assert_eq!(frame.anim, CharacterMotorAnim::StrafeRight);
        assert!(!frame.sprinting, "no sprint sideways under lock");
        assert_eq!(frame.position.x, config().walk_speed);
    }

    #[test]
    fn locked_evade_rolls_in_every_direction_without_dropping_lock_input() {
        // Diagonals resolve by the same sectors as locked locomotion:
        // within a quarter-turn of the facing counts as forward, the
        // mirrored sector as backward, the rest as side slides.
        let directions = [
            (Q12::ZERO, Q12::ONE, CharacterMotorAnim::Roll),
            (Q12::ZERO, Q12::NEG_ONE, CharacterMotorAnim::Quickstep),
            (Q12::NEG_ONE, Q12::ZERO, CharacterMotorAnim::DashLeft),
            (Q12::ONE, Q12::ZERO, CharacterMotorAnim::DashRight),
            (Q12::ONE, Q12::ONE, CharacterMotorAnim::Roll),
            (Q12::NEG_ONE, Q12::ONE, CharacterMotorAnim::Roll),
            (Q12::ONE, Q12::NEG_ONE, CharacterMotorAnim::Quickstep),
            (Q12::NEG_ONE, Q12::NEG_ONE, CharacterMotorAnim::Quickstep),
        ];
        for (move_x, move_z, expected) in directions {
            let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::HALF);
            let frame = motor.update(
                None,
                CharacterMotorInput {
                    move_x,
                    move_z,
                    facing_yaw: Some(Angle::ZERO),
                    evade: true,
                    ..CharacterMotorInput::default()
                },
                config(),
            );
            assert_eq!(frame.action, CharacterMotorAction::Roll);
            assert_eq!(frame.anim, expected);
            // The body no longer turns into the slide; it holds its
            // current combat facing while the motor travels the input
            // direction.
            assert_eq!(frame.yaw, Angle::HALF);
            assert!(frame.moved);
            assert_eq!(motor.action_yaw, yaw_from_vector(move_x, move_z));
        }
    }

    #[test]
    fn neutral_locked_evade_rolls_toward_target() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::HALF);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                facing_yaw: Some(Angle::ZERO),
                evade: true,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.action, CharacterMotorAction::Roll);
        assert_eq!(frame.anim, CharacterMotorAnim::Roll);
        assert_eq!(frame.yaw, Angle::ZERO);
        assert_eq!(frame.position, RoomPoint::new(0, 0, 96));
    }

    #[test]
    fn analog_vector_scales_speed_by_magnitude() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                move_x: Q12::HALF,
                move_z: Q12::ZERO,
                walk: 1,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.position, RoomPoint::new(8, 0, 0));
        assert_eq!(frame.yaw, Angle::QUARTER);
    }

    #[test]
    fn actor_cylinder_blocks_horizontal_overlap() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut cfg = config();
        cfg.walk_speed = 160;
        cfg.height = 768;
        let blockers = [CharacterCollisionCylinder::new(
            RoomPoint::new(0, 0, 160),
            64,
            768,
        )];
        let frame = motor.update_vblanks_with_collision(
            CharacterCollision::new(None, &blockers),
            CharacterMotorInput {
                walk: 1,
                ..CharacterMotorInput::default()
            },
            cfg,
            1,
        );
        assert_eq!(frame.position, RoomPoint::ZERO);
        assert!(!frame.moved);
        assert!(frame.blocked);
    }

    #[test]
    fn actor_cylinder_ignores_vertical_gap() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut cfg = config();
        cfg.walk_speed = 160;
        cfg.height = 256;
        let blockers = [CharacterCollisionCylinder::new(
            RoomPoint::new(0, 512, 160),
            64,
            256,
        )];
        let frame = motor.update_vblanks_with_collision(
            CharacterCollision::new(None, &blockers),
            CharacterMotorInput {
                walk: 1,
                ..CharacterMotorInput::default()
            },
            cfg,
            1,
        );
        assert_eq!(frame.position, RoomPoint::new(0, 0, 160));
        assert!(frame.moved);
        assert!(!frame.blocked);
    }

    #[test]
    fn actor_aabb_blocks_horizontal_overlap() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut cfg = config();
        cfg.walk_speed = 160;
        cfg.height = 768;
        let blockers = [CharacterCollisionAabb::new(
            RoomPoint::new(-64, 0, 96),
            RoomPoint::new(64, 768, 224),
        )];
        let frame = motor.update_vblanks_with_collision(
            CharacterCollision::new_with_aabbs(None, &[], &blockers),
            CharacterMotorInput {
                walk: 1,
                ..CharacterMotorInput::default()
            },
            cfg,
            1,
        );
        assert_eq!(frame.position, RoomPoint::ZERO);
        assert!(!frame.moved);
        assert!(frame.blocked);
    }

    #[test]
    fn solid_wall_between_walkable_sectors_blocks_cylinder() {
        let bytes = world_with_internal_south_wall();
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let mut motor = CharacterMotorState::new(RoomPoint::new(512, 0, 800), Angle::ZERO);
        let mut cfg = config();
        cfg.walk_speed = 288;
        cfg.height = 768;
        let frame = motor.update(
            Some(room.collision()),
            CharacterMotorInput {
                walk: 1,
                ..CharacterMotorInput::default()
            },
            cfg,
        );
        assert_eq!(frame.position, RoomPoint::new(512, 0, 800));
        assert!(!frame.moved);
        assert!(frame.blocked);
    }

    #[test]
    fn stacked_floor_reports_elevation_in_current_space() {
        // A collision room offset up by 3584 engine units (an upper floor)
        // must report its floor at the current-space height 3584, not its
        // room-local 0. Without the offset_y handling the motor would see
        // the upper floor at Y=0 and never let the player step up onto it.
        let bytes = flat_floor_world();
        let upper = RuntimeRoom::from_bytes(&bytes).expect("upper room parses");
        let rooms = [CharacterCollisionRoom::new(upper, 0, 0).with_offset_y(3584)];
        // Query a cell inside the room footprint.
        let h = floor_height_at_rooms(&rooms, 512, 512);
        assert_eq!(h, Some(3584), "upper floor must report its true elevation");
    }

    #[test]
    fn ground_floor_unaffected_by_zero_offset() {
        // offset_y defaults to 0, so a ground room is byte-identical to
        // before: floor at room-local 0.
        let bytes = flat_floor_world();
        let ground = RuntimeRoom::from_bytes(&bytes).expect("ground room parses");
        let rooms = [CharacterCollisionRoom::new(ground, 0, 0)];
        assert_eq!(floor_height_at_rooms(&rooms, 512, 512), Some(0));
    }

    #[test]
    fn resolve_step_down_snaps_small_drops_but_drops_into_falls() {
        // Steps up always snap to the floor (gravity never lifts the feet).
        assert_eq!(resolve_step_down(0, 640), 640);
        // A drop within STEP_DOWN_HEIGHT snaps straight down (descending a step).
        assert_eq!(resolve_step_down(0, -STEP_DOWN_HEIGHT), -STEP_DOWN_HEIGHT);
        assert_eq!(
            resolve_step_down(0, -STEP_DOWN_HEIGHT + 1),
            -STEP_DOWN_HEIGHT + 1
        );
        // One unit deeper than a step keeps the feet put: the body walks out
        // over the ledge at its current height and gravity takes over.
        assert_eq!(resolve_step_down(0, -STEP_DOWN_HEIGHT - 1), 0);
        assert_eq!(resolve_step_down(0, -3584), 0);
    }

    #[test]
    fn airborne_body_falls_gradually_and_lands_on_floor() {
        // A flat floor at y=0; spawn the body high above it and apply no
        // input. Gravity must pull it down over several frames (not teleport)
        // and settle it exactly on the floor without passing through.
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("room parses");
        let rooms = [CharacterCollisionRoom::new(room, 0, 0)];
        let mut motor = CharacterMotorState::new(RoomPoint::new(512, 2048, 512), Angle::ZERO);
        let cfg = config();

        let f1 = motor.update_vblanks_with_collision(
            CharacterCollision::rooms(&rooms, &[]),
            CharacterMotorInput::default(),
            cfg,
            1,
        );
        assert!(f1.position.y < 2048, "gravity pulls the body down");
        assert!(
            f1.position.y > 0,
            "it falls gradually, not teleporting to the floor (y={})",
            f1.position.y
        );

        let mut min_y = f1.position.y;
        let mut rest_y = f1.position.y;
        for _ in 0..60 {
            let f = motor.update_vblanks_with_collision(
                CharacterCollision::rooms(&rooms, &[]),
                CharacterMotorInput::default(),
                cfg,
                1,
            );
            rest_y = f.position.y;
            min_y = min_y.min(f.position.y);
        }
        assert_eq!(rest_y, 0, "lands exactly on the floor");
        assert!(min_y >= 0, "never falls through the floor (min={min_y})");
    }

    #[test]
    fn weight_scales_airborne_gravity() {
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("room parses");
        let rooms = [CharacterCollisionRoom::new(room, 0, 0)];

        let mut normal = CharacterMotorState::new(RoomPoint::new(512, 2048, 512), Angle::ZERO);
        let normal_frame = normal.update_vblanks_with_collision(
            CharacterCollision::rooms(&rooms, &[]),
            CharacterMotorInput::default(),
            config(),
            1,
        );

        let mut heavy_config = config();
        heavy_config.weight_q8 = DEFAULT_WEIGHT_Q8 * 2;
        let mut heavy = CharacterMotorState::new(RoomPoint::new(512, 2048, 512), Angle::ZERO);
        let heavy_frame = heavy.update_vblanks_with_collision(
            CharacterCollision::rooms(&rooms, &[]),
            CharacterMotorInput::default(),
            heavy_config,
            1,
        );

        assert_eq!(normal_frame.position.y, 2048 - GRAVITY_PER_TICK);
        assert_eq!(heavy_frame.position.y, 2048 - GRAVITY_PER_TICK * 2);
    }

    #[test]
    fn airborne_body_falls_to_lower_stacked_floor() {
        // Upper room: cell 0 has floor at y=0, cell 1 is a hole. A lower room
        // sits under the hole, dropped 3584 units. A body standing over the
        // hole must fall through and land on the lower floor at -3584.
        let upper_bytes = sparse_two_sector_world();
        let upper = RuntimeRoom::from_bytes(&upper_bytes).expect("upper parses");
        let lower_bytes = flat_floor_world();
        let lower = RuntimeRoom::from_bytes(&lower_bytes).expect("lower parses");
        let rooms = [
            CharacterCollisionRoom::new(upper, 0, 0),
            CharacterCollisionRoom::new(lower, 1024, 0).with_offset_y(-3584),
        ];
        // x in [1024, 2048) is the hole cell.
        let mut motor = CharacterMotorState::new(RoomPoint::new(1536, 0, 512), Angle::ZERO);
        let cfg = config();
        let mut rest_y = 0;
        let mut min_y = 0;
        for _ in 0..60 {
            let f = motor.update_vblanks_with_collision(
                CharacterCollision::rooms(&rooms, &[]),
                CharacterMotorInput::default(),
                cfg,
                1,
            );
            rest_y = f.position.y;
            min_y = min_y.min(f.position.y);
        }
        assert_eq!(rest_y, -3584, "falls through the hole onto the lower floor");
        assert!(min_y >= -3584, "never falls through the lower floor");
    }

    #[test]
    fn standing_on_upper_stacked_floor_does_not_fall_to_lower() {
        // Regression: the collision lists the LOWER room first, then the upper
        // floor at +3584. A body standing on the upper floor (feet 3584) must
        // rest there, not be pulled down by gravity to the lower floor at 0
        // that occupies the same X/Z. (First-match floor lookup would return
        // the lower floor and drop the player through the upper one.)
        let lower_bytes = flat_floor_world();
        let upper_bytes = flat_floor_world();
        let lower = RuntimeRoom::from_bytes(&lower_bytes).expect("lower parses");
        let upper = RuntimeRoom::from_bytes(&upper_bytes).expect("upper parses");
        let rooms = [
            CharacterCollisionRoom::new(lower, 0, 0),
            CharacterCollisionRoom::new(upper, 0, 0).with_offset_y(3584),
        ];
        let mut motor = CharacterMotorState::new(RoomPoint::new(512, 3584, 512), Angle::ZERO);
        let cfg = config();
        for _ in 0..30 {
            let f = motor.update_vblanks_with_collision(
                CharacterCollision::rooms(&rooms, &[]),
                CharacterMotorInput::default(),
                cfg,
                1,
            );
            assert_eq!(
                f.position.y, 3584,
                "stays on the upper floor, not pulled down to the lower"
            );
        }
    }

    #[test]
    fn solid_wall_in_later_stacked_room_still_blocks() {
        // The lower room overlaps X/Z and is listed first, but the body is on
        // the upper floor. Multi-room wall checks must keep scanning after a
        // lower-room non-hit, otherwise upper-room walls are ignored.
        let lower_bytes = flat_floor_world();
        let upper_bytes = world_with_internal_south_wall();
        let lower = RuntimeRoom::from_bytes(&lower_bytes).expect("lower parses");
        let upper = RuntimeRoom::from_bytes(&upper_bytes).expect("upper parses");
        let rooms = [
            CharacterCollisionRoom::new(lower, 0, 0),
            CharacterCollisionRoom::new(upper, 0, 0).with_offset_y(3584),
        ];

        assert!(body_hits_solid_wall_in_rooms(
            &rooms,
            RoomPoint::new(512, 3584, 1000),
            96,
            768
        ));
    }

    #[test]
    fn idle_body_snaps_within_a_step_then_stays_grounded() {
        // Spawned within STEP_DOWN above a flat floor with no input: the body
        // snaps down onto the floor (not a fall) and then stays grounded tick
        // after tick via the cached fast path.
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("room parses");
        let rooms = [CharacterCollisionRoom::new(room, 0, 0)];
        let mut motor = CharacterMotorState::new(RoomPoint::new(512, 320, 512), Angle::ZERO);
        let cfg = config();
        for i in 0..20 {
            let f = motor.update_vblanks_with_collision(
                CharacterCollision::rooms(&rooms, &[]),
                CharacterMotorInput::default(),
                cfg,
                1,
            );
            assert_eq!(f.position.y, 0, "tick {i}: snapped onto floor and stays");
        }
    }

    #[test]
    fn grounded_walk_keeps_feet_on_flat_floor() {
        // Regression: with gravity in place, ordinary walking on a flat floor
        // still keeps the feet glued at y=0 (no drift, no float).
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("room parses");
        let rooms = [CharacterCollisionRoom::new(room, 0, 0)];
        let mut motor = CharacterMotorState::new(RoomPoint::new(128, 0, 128), Angle::ZERO);
        let mut cfg = config();
        cfg.walk_speed = 64;
        for _ in 0..8 {
            let f = motor.update_vblanks_with_collision(
                CharacterCollision::rooms(&rooms, &[]),
                CharacterMotorInput {
                    walk: 1,
                    ..CharacterMotorInput::default()
                },
                cfg,
                1,
            );
            assert_eq!(f.position.y, 0, "feet stay on the flat floor while walking");
        }
    }

    #[test]
    fn multi_room_collision_crosses_flat_chunk_seam() {
        let bytes_a = flat_floor_world();
        let bytes_b = flat_floor_world();
        let room_a = RuntimeRoom::from_bytes(&bytes_a).expect("room a parses");
        let room_b = RuntimeRoom::from_bytes(&bytes_b).expect("room b parses");
        let rooms = [
            CharacterCollisionRoom::new(room_a, 0, 0),
            CharacterCollisionRoom::new(room_b, 1024, 0),
        ];
        let mut motor = CharacterMotorState::new(RoomPoint::new(960, 0, 512), Angle::QUARTER);
        let mut cfg = config();
        cfg.walk_speed = 128;
        cfg.radius = 96;

        let frame = motor.update_vblanks_with_collision(
            CharacterCollision::rooms(&rooms, &[]),
            CharacterMotorInput {
                walk: 1,
                ..CharacterMotorInput::default()
            },
            cfg,
            1,
        );

        assert_eq!(frame.position, RoomPoint::new(1088, 0, 512));
        assert!(frame.moved);
        assert!(!frame.blocked);
    }

    #[test]
    fn collision_room_membership_ignores_empty_cells_inside_bounds() {
        let bytes = sparse_two_sector_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("sparse room parses");
        let collision = CharacterCollisionRoom::new(room, 0, 0);

        assert!(collision_room_contains_point(
            collision,
            RuntimeCollisionRoom::Runtime(room),
            512,
            512
        ));
        assert!(!collision_room_contains_point(
            collision,
            RuntimeCollisionRoom::Runtime(room),
            1536,
            512
        ));
    }

    #[test]
    fn diagonal_wall_segment_blocks_cylinder_overlap() {
        assert!(circle_overlaps_wall_segment(
            512,
            512,
            64,
            0,
            0,
            1024,
            DIR_NORTH_WEST_SOUTH_EAST
        ));
        assert!(circle_overlaps_wall_segment(
            512,
            512,
            64,
            0,
            0,
            1024,
            DIR_NORTH_EAST_SOUTH_WEST
        ));
        assert!(!circle_overlaps_wall_segment(
            512,
            700,
            64,
            0,
            0,
            1024,
            DIR_NORTH_WEST_SOUTH_EAST
        ));
    }

    #[test]
    fn analog_sprint_reports_run() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                move_x: Q12::ONE,
                move_z: Q12::ZERO,
                walk: 1,
                sprint: true,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.position, RoomPoint::new(64, 0, 0));
        assert_eq!(frame.anim, CharacterMotorAnim::Run);
        assert!(frame.sprinting);
    }

    #[test]
    fn sprint_consumes_stamina_and_reports_run() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                walk: 1,
                sprint: true,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.position, RoomPoint::new(0, 0, 64));
        assert_eq!(frame.anim, CharacterMotorAnim::Run);
        assert!(frame.sprinting);
        assert!(frame.stamina_q12 < DEFAULT_STAMINA_MAX_Q12);
    }

    #[test]
    fn held_sprint_stays_walk_after_exhaustion_until_released() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut cfg = config();
        cfg.stamina_max_q12 = 96;
        cfg.sprint_min_q12 = 32;
        cfg.sprint_drain_q12 = 64;
        cfg.stamina_recover_q12 = 16;
        motor.stamina_q12 = cfg.stamina_max_q12;

        let held = CharacterMotorInput {
            walk: 1,
            sprint: true,
            ..CharacterMotorInput::default()
        };

        let first = motor.update(None, held, cfg);
        let second = motor.update(None, held, cfg);
        assert_eq!(first.anim, CharacterMotorAnim::Run);
        assert_eq!(second.anim, CharacterMotorAnim::Run);
        assert_eq!(second.stamina_q12, 0);

        for _ in 0..4 {
            let frame = motor.update(None, held, cfg);
            assert_eq!(frame.anim, CharacterMotorAnim::Walk);
            assert!(!frame.sprinting);
        }

        let released = motor.update(
            None,
            CharacterMotorInput {
                walk: 1,
                sprint: false,
                ..CharacterMotorInput::default()
            },
            cfg,
        );
        assert_eq!(released.anim, CharacterMotorAnim::Walk);

        let restarted = motor.update(None, held, cfg);
        assert_eq!(restarted.anim, CharacterMotorAnim::Run);
        assert!(restarted.sprinting);
    }

    #[test]
    fn held_sprint_survives_brief_direction_change_idle_gap() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut cfg = config();
        cfg.stamina_max_q12 = 512;
        cfg.sprint_min_q12 = 384;
        cfg.sprint_drain_q12 = 256;
        cfg.stamina_recover_q12 = 0;
        motor.stamina_q12 = cfg.stamina_max_q12;

        let held_run = CharacterMotorInput {
            walk: 1,
            sprint: true,
            ..CharacterMotorInput::default()
        };
        let held_idle = CharacterMotorInput {
            sprint: true,
            ..CharacterMotorInput::default()
        };

        let first = motor.update(None, held_run, cfg);
        assert_eq!(first.anim, CharacterMotorAnim::Run);
        assert_eq!(first.stamina_q12, 256);

        let idle_gap = motor.update(None, held_idle, cfg);
        assert_eq!(idle_gap.anim, CharacterMotorAnim::Idle);

        let resumed = motor.update(None, held_run, cfg);
        assert_eq!(resumed.anim, CharacterMotorAnim::Run);
        assert!(resumed.sprinting);
    }

    #[test]
    fn vblank_delta_matches_repeated_single_frame_updates() {
        let cfg = config();
        let input = CharacterMotorInput {
            walk: 1,
            sprint: true,
            ..CharacterMotorInput::default()
        };
        let mut stepped = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut caught_up = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);

        let _ = stepped.update(None, input, cfg);
        let expected = stepped.update(None, input, cfg);
        let actual = caught_up.update_vblanks(None, input, cfg, 2);

        assert_eq!(actual.position, expected.position);
        assert_eq!(actual.yaw, expected.yaw);
        assert_eq!(actual.anim, expected.anim);
        assert_eq!(actual.stamina_q12, expected.stamina_q12);
        assert_eq!(caught_up.stamina_q12(), stepped.stamina_q12());
    }

    #[test]
    fn vblank_delta_consumes_evade_edge_once() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut cfg = config();
        cfg.roll_cost_q12 = 512;
        cfg.roll_speed = 0;
        cfg.roll_active_frames = 1;
        cfg.roll_recovery_frames = 0;
        cfg.roll_invulnerable_frames = 1;
        cfg.stamina_recover_q12 = 0;
        motor.stamina_q12 = 1024;

        let frame = motor.update_vblanks(
            None,
            CharacterMotorInput {
                walk: 1,
                evade: true,
                ..CharacterMotorInput::default()
            },
            cfg,
            2,
        );

        assert_eq!(frame.anim, CharacterMotorAnim::Walk);
        assert_eq!(frame.action, CharacterMotorAction::Idle);
        assert_eq!(frame.stamina_q12, 512);
    }

    #[test]
    fn evade_starts_roll_with_invulnerability() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                walk: 1,
                evade: true,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.action, CharacterMotorAction::Roll);
        assert_eq!(frame.anim, CharacterMotorAnim::Roll);
        assert!(frame.invulnerable);
        assert_eq!(frame.position, RoomPoint::new(0, 0, 96));
    }

    #[test]
    fn is_action_invulnerable_tracks_the_roll_i_frame_window() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut cfg = config();
        cfg.roll_active_frames = 4;
        cfg.roll_recovery_frames = 4;
        cfg.roll_invulnerable_frames = 3;

        // Idle: never invulnerable.
        assert!(!motor.is_action_invulnerable(cfg));

        let evade = CharacterMotorInput {
            walk: 1,
            evade: true,
            ..CharacterMotorInput::default()
        };
        // Start the roll. Queried between ticks, the accessor reports
        // the invulnerability the NEXT update will apply, so it must
        // agree with that update's frame result for the whole action.
        let frame = motor.update(None, evade, cfg);
        assert!(frame.invulnerable);
        for _ in 0..8 {
            let expected = motor.is_action_invulnerable(cfg);
            let frame = motor.update(None, CharacterMotorInput::default(), cfg);
            assert_eq!(frame.invulnerable, expected);
        }
        // Action finished: back to never-invulnerable.
        assert_eq!(motor.action(), CharacterMotorAction::Idle);
        assert!(!motor.is_action_invulnerable(cfg));
    }

    #[test]
    fn backwards_free_evade_rolls_in_the_requested_direction() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                walk: -1,
                evade: true,
                ..CharacterMotorInput::default()
            },
            config(),
        );
        assert_eq!(frame.action, CharacterMotorAction::Roll);
        assert_eq!(frame.anim, CharacterMotorAnim::Roll);
        assert_eq!(frame.yaw, Angle::HALF);
        assert_eq!(frame.position, RoomPoint::new(0, 0, -96));
    }

    #[test]
    fn body_step_moves_on_open_floor_and_no_clips_without_collision() {
        // On a flat floor the step commits fully.
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("room parses");
        let rooms = [CharacterCollisionRoom::new(room, 0, 0)];
        let step = commit_body_step(
            CharacterCollision::rooms(&rooms, &[]),
            RoomPoint::new(400, 0, 400),
            96,
            32,
            64,
            768,
        );
        assert_eq!(step.position, RoomPoint::new(496, 0, 432));
        assert!(step.moved);
        assert!(!step.blocked);
        // With no collision wired at all, the step passes through
        // (the player commit's no-room fallback; unit-test shape).
        let step = commit_body_step(
            CharacterCollision::room(None),
            RoomPoint::new(0, 0, 0),
            10,
            -10,
            64,
            768,
        );
        assert_eq!(step.position, RoomPoint::new(10, 0, -10));
        assert!(step.moved);
    }

    #[test]
    fn body_step_blocks_on_solid_wall_like_the_player() {
        // The internal south wall between the two sectors blocks the
        // +Z step, exactly as the player motor test above.
        let bytes = world_with_internal_south_wall();
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let step = commit_body_step(
            CharacterCollision::room(Some(room.collision())),
            RoomPoint::new(512, 0, 800),
            0,
            288,
            64,
            768,
        );
        assert_eq!(step.position, RoomPoint::new(512, 0, 800));
        assert!(!step.moved);
        assert!(step.blocked);
    }

    #[test]
    fn body_step_slides_along_the_blocked_axis() {
        // Diagonal step into the same south wall: Z is rejected, X
        // slides (the player commit's axis cascade).
        let bytes = world_with_internal_south_wall();
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let step = commit_body_step(
            CharacterCollision::room(Some(room.collision())),
            RoomPoint::new(400, 0, 800),
            96,
            288,
            64,
            768,
        );
        assert_eq!(step.position, RoomPoint::new(496, 0, 800));
        assert!(step.moved);
        assert!(step.blocked);
    }

    #[test]
    fn body_step_refuses_to_leave_the_walkable_grid() {
        // One walkable sector (1024x1024): stepping past its edge finds
        // no floor and is rejected -- an AI body never walks into the
        // void the way a player can walk off a ledge and fall.
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("room parses");
        let rooms = [CharacterCollisionRoom::new(room, 0, 0)];
        let step = commit_body_step(
            CharacterCollision::rooms(&rooms, &[]),
            RoomPoint::new(900, 0, 512),
            600,
            0,
            32,
            768,
        );
        assert_eq!(step.position, RoomPoint::new(900, 0, 512));
        assert!(!step.moved);
        assert!(step.blocked);
    }

    #[test]
    fn body_step_respects_cylinder_and_aabb_blockers() {
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("room parses");
        let rooms = [CharacterCollisionRoom::new(room, 0, 0)];
        let cylinders = [CharacterCollisionCylinder::new(
            RoomPoint::new(512, 0, 700),
            64,
            768,
        )];
        let step = commit_body_step(
            CharacterCollision::rooms(&rooms, &cylinders),
            RoomPoint::new(512, 0, 500),
            0,
            160,
            64,
            768,
        );
        assert_eq!(
            step.position,
            RoomPoint::new(512, 0, 500),
            "cylinder blocker rejects the step"
        );
        assert!(step.blocked);

        let aabbs = [CharacterCollisionAabb::new(
            RoomPoint::new(400, 0, 600),
            RoomPoint::new(624, 768, 700),
        )];
        let step = commit_body_step(
            CharacterCollision::rooms_with_aabbs(&rooms, &[], &aabbs),
            RoomPoint::new(512, 0, 480),
            0,
            160,
            32,
            768,
        );
        assert_eq!(
            step.position,
            RoomPoint::new(512, 0, 480),
            "aabb blocker rejects the step"
        );
        assert!(step.blocked);
    }
}
