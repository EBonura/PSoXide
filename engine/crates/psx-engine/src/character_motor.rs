//! Fixed-budget third-person character motor.
//!
//! The motor owns player locomotion state that should be shared by
//! game code and AI controllers: position, yaw, stamina, short evade
//! actions, and the coarse collision commit against cooked room data.
//! Inputs are intent-shaped rather than pad-shaped so callers can feed
//! either player controls or future behaviour-tree output.

use crate::floor_sample::{height_at_local, triangle_heights_to_quad};
use crate::{
    fixed::div_q12_i32, Angle, RoomCollision, RoomPoint, RuntimeCollisionRoom, RuntimeRoom, Q12,
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
        let config = normalize_config(config);
        let steps = delta_vblanks.clamp(1, MAX_MOTOR_CATCHUP_VBLANKS);
        let mut final_frame: Option<CharacterMotorFrame> = None;

        for step in 0..steps {
            let mut step_input = input;
            if step > 0 {
                step_input.evade = false;
            }
            let frame = self.update_one_frame(collision, step_input, config);
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

        final_frame.unwrap_or_else(|| {
            self.frame(
                CharacterMotorAnim::Idle,
                self.action,
                false,
                false,
                false,
                false,
                false,
            )
        })
    }

    fn update_one_frame(
        &mut self,
        collision: CharacterCollision<'_, '_, '_>,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
    ) -> CharacterMotorFrame {
        self.stamina_q12 = self.stamina_q12.clamp(0, config.stamina_max_q12);
        self.apply_vertical(&collision, config);

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
            // Sprint intent remains authoritative during lock-on in every
            // movement direction. Walking keeps the combat-facing stance;
            // sprinting temporarily faces travel while the camera and target
            // lock remain active, matching the modern Souls control contract.
            let wants_sprint = input.sprint;
            self.update_sprint_gate(wants_sprint);
            let sprinting = self.can_sprint(wants_sprint, config);
            let base_speed = if sprinting {
                config.run_speed
            } else {
                config.walk_speed
            };
            let speed = move_mag.mul_i32(base_speed);
            if sprinting {
                self.yaw = move_yaw;
            }
            let (moved, blocked) = self.try_move_vector(
                collision,
                move_x,
                move_z,
                speed,
                config.radius,
                config.height,
            );

            if sprinting && moved {
                self.spend_sprint_stamina(config);
            } else {
                self.recover_stamina(config);
            }

            let anim = if !moved && blocked {
                CharacterMotorAnim::Idle
            } else if sprinting {
                CharacterMotorAnim::Run
            } else {
                directional_anim
            };

            return self.frame(
                anim,
                CharacterMotorAction::Idle,
                moved,
                blocked,
                sprinting,
                false,
                false,
            );
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
            self.try_move(collision, signed_speed, config.radius, config.height)
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

        self.frame(
            anim,
            CharacterMotorAction::Idle,
            moved,
            blocked,
            sprinting,
            false,
            false,
        )
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

    fn update_action(
        &mut self,
        collision: CharacterCollision<'_, '_, '_>,
        config: CharacterMotorConfig,
    ) -> CharacterMotorFrame {
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
            )
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
        self.frame(anim, action, moved, blocked, false, invulnerable, recovery)
    }

    fn try_move(
        &mut self,
        collision: CharacterCollision<'_, '_, '_>,
        signed_speed: i32,
        radius: i32,
        height: i32,
    ) -> (bool, bool) {
        self.try_move_at_yaw(collision, self.yaw, signed_speed, radius, height)
    }

    fn try_move_at_yaw(
        &mut self,
        collision: CharacterCollision<'_, '_, '_>,
        yaw: Angle,
        signed_speed: i32,
        radius: i32,
        height: i32,
    ) -> (bool, bool) {
        if signed_speed == 0 {
            return (false, false);
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

    fn try_move_vector(
        &mut self,
        collision: CharacterCollision<'_, '_, '_>,
        move_x: Q12,
        move_z: Q12,
        speed: i32,
        radius: i32,
        height: i32,
    ) -> (bool, bool) {
        if speed == 0 {
            return (false, false);
        }
        let dx = move_x.mul_i32(speed);
        let dz = move_z.mul_i32(speed);
        if dx == 0 && dz == 0 {
            return (false, false);
        }
        let target = RoomPoint::new(
            self.position.x.saturating_add(dx),
            self.position.y,
            self.position.z.saturating_add(dz),
        );
        self.try_commit_move(collision, target, radius, height)
    }

    fn try_commit_move(
        &mut self,
        collision: CharacterCollision<'_, '_, '_>,
        target: RoomPoint,
        radius: i32,
        height: i32,
    ) -> (bool, bool) {
        if let Some(position) = body_stand_position(collision, target, radius, height) {
            self.position = position;
            return (true, false);
        }

        let start = self.position;
        let x_only = RoomPoint::new(target.x, start.y, start.z);
        if let Some(position) = body_stand_position(collision, x_only, radius, height) {
            self.position = position;
            return (position.x != start.x || position.z != start.z, true);
        }

        let z_only = RoomPoint::new(start.x, start.y, target.z);
        if let Some(position) = body_stand_position(collision, z_only, radius, height) {
            self.position = position;
            return (position.x != start.x || position.z != start.z, true);
        }

        // `apply_vertical` validated and anchored this exact X/Z at the start
        // of the tick. When every candidate is blocked, keep that known-good
        // grounded position instead of repeating two full floor/wall scans.
        // The recovery probes below remain for airborne/no-floor edge cases.
        if self.grounded {
            return (false, true);
        }

        if body_stand_position(collision, start, radius, height).is_some() {
            return (false, true);
        }

        if collision.room.is_none()
            && collision.blockers.is_empty()
            && collision.aabb_blockers.is_empty()
        {
            self.position = target;
            return (true, false);
        }

        if target == start {
            return (false, false);
        }
        let Some(position) = body_stand_position(collision, start, 0, height) else {
            return (false, true);
        };
        self.position = position;
        (false, true)
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
    fn apply_vertical(
        &mut self,
        collision: &CharacterCollision<'_, '_, '_>,
        config: CharacterMotorConfig,
    ) {
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
            return;
        }

        // Cold path: resolve the supporting floor (highest floor at/below the
        // feet plus a step, across the active rooms).
        let Some(floor) =
            supporting_floor_height(collision, self.position.x, self.position.z, self.position.y)
        else {
            // No floor anywhere below (open void): hold rather than fall
            // forever. Matches the legacy no-room behaviour.
            self.grounded = false;
            self.velocity_y = 0;
            return;
        };

        if self.position.y.saturating_sub(floor) <= STEP_DOWN_HEIGHT {
            // On the floor, or within a step of it: snap down and ground.
            // Caching the cell lets the next idle tick take the fast path.
            self.position.y = floor;
            self.velocity_y = 0;
            self.set_grounded(floor);
            return;
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

    fn config() -> CharacterMotorConfig {
        CharacterMotorConfig::character(64, 32, 64, Angle::from_q12(16))
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
    fn locked_sprint_applies_to_every_movement_direction() {
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
            let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
            let frame = motor.update(
                None,
                CharacterMotorInput {
                    move_x,
                    move_z,
                    facing_yaw: Some(Angle::ZERO),
                    sprint: true,
                    ..CharacterMotorInput::default()
                },
                config(),
            );
            assert_eq!(frame.yaw, yaw_from_vector(move_x, move_z));
            assert_eq!(frame.anim, CharacterMotorAnim::Run);
            assert!(frame.sprinting);
            assert!(frame.moved);
        }
    }

    #[test]
    fn locked_forward_stick_can_sprint_while_camera_aligns_to_target() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let frame = motor.update(
            None,
            CharacterMotorInput {
                // Camera-relative forward can be lateral to the target for a
                // few frames immediately after lock-on is acquired.
                walk: 1,
                move_x: Q12::ONE,
                facing_yaw: Some(Angle::ZERO),
                sprint: true,
                ..CharacterMotorInput::default()
            },
            config(),
        );

        assert_eq!(frame.yaw, Angle::QUARTER);
        assert_eq!(frame.anim, CharacterMotorAnim::Run);
        assert!(frame.sprinting);
        assert_eq!(frame.position.x, config().run_speed);
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
