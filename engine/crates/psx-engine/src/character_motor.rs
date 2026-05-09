//! Fixed-budget third-person character motor.
//!
//! The motor owns player locomotion state that should be shared by
//! game code and AI controllers: position, yaw, stamina, short evade
//! actions, and the coarse collision commit against cooked room data.
//! Inputs are intent-shaped rather than pad-shaped so callers can feed
//! either player controls or future behaviour-tree output.

use crate::{Angle, RoomCollision, RoomPoint, RuntimeRoom, Q12};

const DEFAULT_STAMINA_MAX_Q12: i32 = 4096;
const DEFAULT_BODY_HEIGHT: i32 = 768;
const MAX_MOTOR_CATCHUP_VBLANKS: u16 = 4;
const SPLIT_NE_SW: u8 = 1;
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

/// One room collision view placed in the motor's current local
/// coordinate space.
///
/// Chunked levels keep the player expressed in the current chunk's
/// room-local coordinates. Adjacent chunks are therefore queried by
/// subtracting their offset from that same current-space point.
#[derive(Copy, Clone, Debug)]
pub struct CharacterCollisionRoom<'room> {
    /// Runtime room/chunk handle.
    pub room: Option<RuntimeRoom<'room>>,
    /// Offset from the motor's current room origin to this room's
    /// origin, in engine units.
    pub offset_x: i32,
    /// Offset from the motor's current room origin to this room's
    /// origin, in engine units.
    pub offset_z: i32,
}

impl<'room> CharacterCollisionRoom<'room> {
    /// Empty non-colliding placeholder for fixed stack buffers.
    pub const EMPTY: Self = Self {
        room: None,
        offset_x: 0,
        offset_z: 0,
    };

    /// Build a collision room with a current-space origin offset.
    pub const fn new(room: RuntimeRoom<'room>, offset_x: i32, offset_z: i32) -> Self {
        Self {
            room: Some(room),
            offset_x,
            offset_z,
        }
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
        }
    }

    /// Build a context that only checks room geometry.
    pub const fn room(room: Option<RoomCollision<'room, 'room_ref>>) -> Self {
        Self {
            room,
            rooms: &[],
            blockers: &[],
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
    /// Stamina spent to start a backstep.
    pub backstep_cost_q12: i32,
    /// Backstep travel speed in world units per display frame.
    pub backstep_speed: i32,
    /// Display frames where backstep keeps moving.
    pub backstep_active_frames: u8,
    /// Recovery display frames after backstep movement ends.
    pub backstep_recovery_frames: u8,
    /// Backstep invulnerability display frames from action start.
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
            stamina_max_q12: DEFAULT_STAMINA_MAX_Q12,
            sprint_min_q12: 384,
            sprint_drain_q12: 48,
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
    /// True while the actor wants to spend stamina on sprinting.
    pub sprint: bool,
    /// Rising-edge evade request. The motor chooses roll vs backstep
    /// from the current walk intent.
    pub evade: bool,
}

/// Current high-level action.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CharacterMotorAction {
    /// No fixed action is currently playing.
    Idle,
    /// Forward evasive roll.
    Roll,
    /// Backward evasive step.
    Backstep,
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
    /// Sprinting.
    Run,
    /// Forward evasive roll.
    Roll,
    /// Backward evasive step.
    Backstep,
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
    /// Sprint is latched while the button stays held so
    /// `sprint_min_q12` means "minimum to start", not "minimum to
    /// continue".
    sprint_latched: bool,
    /// Prevents held-sprint from pulsing Run/Walk every recovery
    /// frame after stamina reaches zero.
    sprint_exhausted: bool,
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
            sprint_latched: false,
            sprint_exhausted: false,
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
    }

    /// Move the motor to another coordinate space while preserving
    /// yaw, stamina, and any in-progress action. Used by streaming
    /// room transitions where the same physical player position is
    /// re-expressed relative to a newly-current chunk.
    pub fn relocate(&mut self, position: RoomPoint) {
        self.position = position;
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
        let steps = delta_vblanks.max(1).min(MAX_MOTOR_CATCHUP_VBLANKS);
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
        self.snap_floor(collision.room, config.radius);

        if self.action.is_idle() && input.evade {
            self.try_start_evade(input, config);
        }

        if !self.action.is_idle() {
            return self.update_action(collision, config);
        }

        if let Some((move_x, move_z, move_mag)) = analog_move_vector(input) {
            self.update_sprint_gate(input.sprint);
            self.yaw = yaw_from_vector(move_x, move_z);
            let wants_sprint = input.sprint;
            let sprinting = self.can_sprint(wants_sprint, config);
            let base_speed = if sprinting {
                config.run_speed
            } else {
                config.walk_speed
            };
            let speed = move_mag.mul_i32(base_speed);
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
                CharacterMotorAnim::Walk
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

    fn try_start_evade(&mut self, input: CharacterMotorInput, config: CharacterMotorConfig) {
        let analog = analog_move_vector(input);
        if let Some((move_x, move_z, _)) = analog {
            self.yaw = yaw_from_vector(move_x, move_z);
        }
        let action = if analog.is_none() && input.walk < 0 {
            CharacterMotorAction::Backstep
        } else {
            CharacterMotorAction::Roll
        };
        let cost = match action {
            CharacterMotorAction::Idle => 0,
            CharacterMotorAction::Roll => config.roll_cost_q12,
            CharacterMotorAction::Backstep => config.backstep_cost_q12,
        };
        if self.stamina_q12 < cost {
            return;
        }
        self.stamina_q12 -= cost;
        self.action = action;
        self.action_frame = 0;
        self.action_yaw = self.yaw;
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
            CharacterMotorAction::Roll => CharacterMotorAnim::Roll,
            CharacterMotorAction::Backstep => CharacterMotorAnim::Backstep,
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

        let z_first = RoomPoint::new(start.x, start.y, target.z);
        if let Some(position) = body_stand_position(collision, z_first, radius, height) {
            let x_after = RoomPoint::new(target.x, position.y, position.z);
            if let Some(position) = body_stand_position(collision, x_after, radius, height) {
                self.position = position;
                return (position.x != start.x || position.z != start.z, true);
            }
        }

        let x_first = RoomPoint::new(target.x, start.y, start.z);
        if let Some(position) = body_stand_position(collision, x_first, radius, height) {
            let z_after = RoomPoint::new(position.x, position.y, target.z);
            if let Some(position) = body_stand_position(collision, z_after, radius, height) {
                self.position = position;
                return (position.x != start.x || position.z != start.z, true);
            }
        }

        if body_stand_position(collision, start, radius, height).is_some() {
            return (false, true);
        }

        if collision.room.is_none() && collision.blockers.is_empty() {
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

    fn snap_floor(&mut self, collision: Option<RoomCollision<'_, '_>>, radius: i32) {
        let Some(room) = collision else {
            return;
        };
        if let Some(height) = stand_height(room, self.position.x, self.position.z, radius) {
            self.position.y = height;
        }
    }

    #[allow(clippy::too_many_arguments)]
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
            CharacterMotorAction::Backstep => Self {
                speed: config.backstep_speed,
                direction: -1,
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

fn body_stand_position(
    collision: CharacterCollision<'_, '_, '_>,
    target: RoomPoint,
    radius: i32,
    height: i32,
) -> Option<RoomPoint> {
    let radius = radius.max(0);
    let height = height.max(1);
    let position = if !collision.rooms.is_empty() {
        let floor = stand_height_in_rooms(collision.rooms, target.x, target.z, radius)?;
        let position = target.with_y(floor);
        if body_hits_solid_wall_in_rooms(collision.rooms, position, radius, height) {
            return None;
        }
        position
    } else {
        match collision.room {
            Some(room) => {
                let floor = stand_height(room, target.x, target.z, radius)?;
                let position = target.with_y(floor);
                if body_hits_solid_wall(room, position, radius, height) {
                    return None;
                }
                position
            }
            None => target,
        }
    };
    if body_hits_blocker(position, radius, height, collision.blockers) {
        return None;
    }
    Some(position)
}

fn stand_height_in_rooms(
    rooms: &[CharacterCollisionRoom<'_>],
    x: i32,
    z: i32,
    radius: i32,
) -> Option<i32> {
    let height = floor_height_at_rooms(rooms, x, z)?;
    if radius <= 0 {
        return Some(height);
    }
    let r = radius.max(0);
    floor_height_at_rooms(rooms, x.saturating_sub(r), z)?;
    floor_height_at_rooms(rooms, x.saturating_add(r), z)?;
    floor_height_at_rooms(rooms, x, z.saturating_sub(r))?;
    floor_height_at_rooms(rooms, x, z.saturating_add(r))?;
    Some(height)
}

fn floor_height_at_rooms(rooms: &[CharacterCollisionRoom<'_>], x: i32, z: i32) -> Option<i32> {
    for collision_room in rooms {
        let Some(room) = collision_room.room else {
            continue;
        };
        if !collision_room_contains_point(*collision_room, room, x, z) {
            continue;
        }
        if let Some(height) = floor_height_at(
            room.collision(),
            x.saturating_sub(collision_room.offset_x),
            z.saturating_sub(collision_room.offset_z),
        ) {
            return Some(height);
        }
    }
    None
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
            position.y,
            position.z.saturating_sub(collision_room.offset_z),
        );
        return body_hits_solid_wall(room.collision(), local_position, radius, height);
    }
    false
}

fn collision_room_contains_point(
    collision_room: CharacterCollisionRoom<'_>,
    room: RuntimeRoom<'_>,
    x: i32,
    z: i32,
) -> bool {
    let Some((x0, x1, z0, z1)) = collision_room_bounds(collision_room, room) else {
        return false;
    };
    x >= x0 && x < x1 && z >= z0 && z < z1
}

fn collision_room_bounds(
    collision_room: CharacterCollisionRoom<'_>,
    room: RuntimeRoom<'_>,
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

fn square_i32_saturating(value: i32) -> i32 {
    let abs = abs_i32(value);
    if abs > 46_340 {
        return i32::MAX;
    }
    abs * abs
}

fn isqrt_i32(value: i32) -> i32 {
    if value <= 0 {
        return 0;
    }
    let mut x = value as u32;
    let mut r = 0u32;
    let mut bit = 1u32 << 30;
    while bit > x {
        bit >>= 2;
    }
    while bit != 0 {
        if x >= r + bit {
            x -= r + bit;
            r = (r >> 1) + bit;
        } else {
            r >>= 1;
        }
        bit >>= 2;
    }
    r as i32
}

fn abs_i32(value: i32) -> i32 {
    if value == i32::MIN {
        i32::MAX
    } else if value < 0 {
        -value
    } else {
        value
    }
}

fn stand_height(room: RoomCollision<'_, '_>, x: i32, z: i32, radius: i32) -> Option<i32> {
    let height = floor_height_at(room, x, z)?;
    if radius <= 0 {
        return Some(height);
    }
    let r = radius.max(0);
    floor_height_at(room, x.saturating_sub(r), z)?;
    floor_height_at(room, x.saturating_add(r), z)?;
    floor_height_at(room, x, z.saturating_sub(r))?;
    floor_height_at(room, x, z.saturating_add(r))?;
    Some(height)
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
    let sector = room.sector(sx as u16, sz as u16)?;
    if !sector.has_floor() {
        return None;
    }
    let local_x = (x - sx * s).clamp(0, s);
    let local_z = (z - sz * s).clamp(0, s);
    let triangle = triangle_index_at_local(sector.floor_split(), local_x, local_z, s);
    if !sector.floor_triangle_walkable(triangle) {
        return None;
    }
    let heights = triangle_heights_to_quad(
        sector.floor_heights(),
        sector.floor_split(),
        triangle,
        sector.floor_triangle_heights(triangle),
    );
    Some(height_at_local(
        heights,
        sector.floor_split(),
        local_x,
        local_z,
        s,
    ))
}

fn triangle_index_at_local(split: u8, local_x: i32, local_z: i32, sector: i32) -> usize {
    psx_asset::world_topology::horizontal_triangle_at_local(split, local_x, local_z, sector)
}

fn triangle_heights_to_quad(
    mut fallback: [i32; 4],
    split: u8,
    triangle: usize,
    heights: [i32; 3],
) -> [i32; 4] {
    let corners = psx_asset::world_topology::split_triangles(split)[triangle.min(1)];
    fallback[corners[0]] = heights[0];
    fallback[corners[1]] = heights[1];
    fallback[corners[2]] = heights[2];
    fallback
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
            if let Some(sector) = room.sector(sx as u16, sz as u16) {
                let mut i = 0;
                while i < sector.wall_count() {
                    if let Some(wall) = room.sector_wall(sector, i) {
                        if wall.solid()
                            && vertical_ranges_overlap(position.y, height, wall.heights())
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
    let dot = (wx as i64)
        .saturating_mul(vx as i64)
        .saturating_add((wz as i64).saturating_mul(vz as i64));
    let t_q12 = (dot
        .saturating_mul(Q12::SCALE as i64)
        .checked_div(len_sq as i64)
        .unwrap_or(0))
    .clamp(0, Q12::SCALE as i64) as i32;
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

fn height_at_local(heights: [i32; 4], split: u8, local_x: i32, local_z: i32, sector: i32) -> i32 {
    let u = local_x.clamp(0, sector);
    let v = local_z.clamp(0, sector);
    let [nw, ne, se, sw] = heights;
    if split == SPLIT_NE_SW {
        if u + v <= sector {
            nw.saturating_add(mul_sector(ne.saturating_sub(nw), u, sector))
                .saturating_add(mul_sector(sw.saturating_sub(nw), v, sector))
        } else {
            sw.saturating_add(mul_sector(se.saturating_sub(sw), u, sector))
                .saturating_add(mul_sector(ne.saturating_sub(se), sector - v, sector))
        }
    } else if v <= u {
        nw.saturating_add(mul_sector(ne.saturating_sub(nw), u - v, sector))
            .saturating_add(mul_sector(se.saturating_sub(nw), v, sector))
    } else {
        nw.saturating_add(mul_sector(se.saturating_sub(sw), u, sector))
            .saturating_add(mul_sector(sw.saturating_sub(nw), v, sector))
    }
}

fn mul_sector(delta: i32, amount: i32, sector: i32) -> i32 {
    if sector <= 0 {
        0
    } else {
        delta.saturating_mul(amount) / sector
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeRoom;

    fn config() -> CharacterMotorConfig {
        CharacterMotorConfig::character(64, 32, 64, Angle::from_q12(16))
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
    fn backwards_evade_starts_backstep() {
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
        assert_eq!(frame.action, CharacterMotorAction::Backstep);
        assert_eq!(frame.anim, CharacterMotorAnim::Backstep);
        assert_eq!(frame.position, RoomPoint::new(0, 0, -72));
    }
}
