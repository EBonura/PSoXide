//! Fixed-budget third-person character motor.
//!
//! The motor owns player locomotion state that should be shared by
//! game code and AI controllers: position, yaw, stamina, short evade
//! actions, and the coarse collision commit against cooked room data.
//! Inputs are intent-shaped rather than pad-shaped so callers can feed
//! either player controls or future behaviour-tree output.

use crate::{
    collision_query::{
        trace_collision, CollisionQueryError, CollisionTrace, CollisionTraceProvider,
        CollisionTraceQuery, CollisionTraceShape, COLLISION_FRACTION_ONE_Q12,
    },
    fixed::div_q12_i32,
    Angle, RoomPoint, Q12,
};
use psx_math::int32::{abs_i32, isqrt_i32, square_i32_saturating};

const DEFAULT_STAMINA_MAX_Q12: i32 = 4096;
const DEFAULT_BODY_HEIGHT: i32 = 48;
/// Max height (engine units) of a wall the character steps over instead
/// of being blocked by. A riser whose top is within this of the feet is
/// treated as a step (the floor probe already found walkable floor on the
/// far side, so the body simply rises onto it); taller walls still block.
/// Sits in the gap between demo-scale steps (<=~576) and real walls
/// (>=~1152).
const STEP_UP_HEIGHT: i32 = 40;
/// Smallest upward normal component, Q12, that a surface may have and still be
/// stood on.
///
/// Quake's rule: a plane is floor when its upward normal component is at least
/// 0.7, roughly 45.6 degrees, and anything steeper is a wall the body slides
/// down instead of climbing. The tests here only asked for a *positive*
/// upward component, so a near-vertical face with a normal of 0.01 up counted
/// as walkable floor and the body could ascend an almost sheer brush.
///
/// 0.7 * 4096 = 2867.
const MIN_WALKABLE_FLOOR_NORMAL_Y_Q12: i16 = 2867;
/// Largest drop the feet snap straight down to (the descent counterpart of
/// [`STEP_UP_HEIGHT`]): walking down demo-scale steps stays glued to the
/// floor. A larger drop -- a ledge, or a hole over a lower floor -- leaves
/// the body airborne instead, and [`CharacterMotorState::apply_vertical`]
/// lets it fall.
const STEP_DOWN_HEIGHT: i32 = 40;
/// Downward acceleration applied to an airborne body each fixed tick, in
/// engine units per tick^2. Integer fixed-point (the PS1 has no FPU).
const GRAVITY_PER_TICK: i32 = 6;
/// Q8 gravity multiplier representing 1.0x body weight.
const DEFAULT_WEIGHT_Q8: u16 = 256;
const MIN_WEIGHT_Q8: u16 = 1;
const MAX_WEIGHT_Q8: u16 = 4096;
/// Terminal downward speed in engine units per tick, so a long fall stays
/// bounded and deterministic.
const MAX_FALL_SPEED: i32 = 48;
const MAX_MOTOR_CATCHUP_VBLANKS: u16 = 4;
/// Maximum downward BSP/provider probe in engine world units.
const TRACE_FLOOR_PROBE_DOWN: i32 = 32_767;
/// Lift a grounded trace origin clear of the supporting plane's epsilon band.
const TRACE_FLOOR_PROBE_LIFT: i32 = 1;
/// Precise first-stage floor probe. A single 32K-unit sweep has only about
/// eight world units per Q0.12 fraction step, so a shallow ramp immediately
/// below the feet can quantize to fraction zero and lose to a lower plane.
/// This range covers both step-down grounding and one terminal-velocity tick;
/// only ledges/falls need the legacy long fallback.
const TRACE_FLOOR_NEAR_PROBE_DOWN: i32 = STEP_DOWN_HEIGHT + MAX_FALL_SPEED + TRACE_FLOOR_PROBE_LIFT;
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
/// a wrapped-provider failure leaves the caller's output untouched. Point
/// queries with prop AABBs also fail without touching output when their Q0.12
/// interpolation product cannot be represented without saturation.
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
    /// Point-query axis deltas must be in `-524288..=524287`, the exact range
    /// whose multiplication by 4096 cannot saturate. A larger or overflowed
    /// delta returns `false` without touching the caller's output.
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

/// Collision traces run with their stack in the scratchpad: the hull
/// walker's explicit continuation stack lives in its frame, so on the RAM
/// stack every push and pop is a main-RAM access. Every scratchpad
/// reservation in the engine lives inside one render function that never
/// traces (the BSP face passes, the blended model vertex chunk), so the stack
/// may use all of the scratchpad.
type TraceStack = crate::scratchpad::ScratchpadStack<0, { crate::scratchpad::SIZE }>;
// Regions live around a trace: none but its own stack.
const _: () = crate::scratchpad::assert_disjoint(&[TraceStack::REGION]);

impl<P: CollisionTraceProvider + ?Sized> CollisionTraceProvider
    for CharacterBlockerTraceProvider<'_, '_, P>
{
    // Out of line so the stack switch is set up once here rather than in
    // every caller.
    #[inline(never)]
    fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool {
        // SAFETY: no scratchpad bytes are live around a trace (see
        // TraceStack), tracing installs no exception handler, and
        // tools/stack_guard.py proves the call tree fits.
        unsafe { TraceStack::run(|| self.trace_into_on_current_stack(query, output)) }
    }
}

impl<P: CollisionTraceProvider + ?Sized> CharacterBlockerTraceProvider<'_, '_, P> {
    #[inline(always)]
    fn trace_into_on_current_stack(
        &mut self,
        query: CollisionTraceQuery,
        output: &mut CollisionTrace,
    ) -> bool {
        if self.aabb_blockers.len() > psx_level::MAX_STATIC_PROP_AABB_BLOCKERS
            || self
                .aabb_blockers
                .iter()
                .any(|blocker| !blocker.is_strictly_valid())
            || (!self.aabb_blockers.is_empty() && !point_aabb_query_is_representable(query))
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

/// Where a body ends one committed step, plus the supporting floor the step
/// itself measured there.
///
/// `grounded_floor` is `Some(floor)` only when the solve proved the body ends
/// the step standing exactly on `floor` at `position`'s XZ, so
/// [`CharacterMotorState::apply_vertical`] can reuse it on the next tick
/// instead of re-tracing the same floor. Backends that cannot prove that
/// report `None`, which keeps the old behaviour of re-querying.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct StandOutcome {
    position: RoomPoint,
    grounded_floor: Option<i32>,
}

impl StandOutcome {
    const fn unmeasured(position: RoomPoint) -> Self {
        Self {
            position,
            grounded_floor: None,
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
    ) -> Result<Option<StandOutcome>, CollisionQueryError>;

    fn air_position(
        &mut self,
        start: RoomPoint,
        target: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<StandOutcome>, CollisionQueryError>;

    fn move_position(
        &mut self,
        start: RoomPoint,
        target: RoomPoint,
        shape: CollisionTraceShape,
        grounded: bool,
    ) -> Result<Option<StandOutcome>, CollisionQueryError> {
        if grounded {
            self.stand_position(start, target, shape)
        } else {
            self.air_position(start, target, shape)
        }
    }

    fn recovery_position(
        &mut self,
        start: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<StandOutcome>, CollisionQueryError>;

    fn has_world_collision(&self) -> bool;
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
    ) -> Result<Option<StandOutcome>, CollisionQueryError> {
        trace_stand_position(self.provider, start, target, shape)
    }

    fn air_position(
        &mut self,
        start: RoomPoint,
        target: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<StandOutcome>, CollisionQueryError> {
        let trace = trace_collision(
            self.provider,
            CollisionTraceQuery {
                start,
                end: target,
                shape,
            },
        )?;
        Ok((!trace.hit()).then_some(StandOutcome::unmeasured(target)))
    }

    fn recovery_position(
        &mut self,
        start: RoomPoint,
        shape: CollisionTraceShape,
    ) -> Result<Option<StandOutcome>, CollisionQueryError> {
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
    /// Forward/backward walking speed in Q8 world units per display frame
    /// (256 = one unit per tick). Sub-unit speeds accumulate in the motor's
    /// per-axis remainder, so partial stick deflection still moves the body.
    pub walk_speed: i32,
    /// Sprint speed in Q8 world units per display frame.
    pub run_speed: i32,
    /// Whether sprint input may enter the Run state.
    ///
    /// Characters without an authored Run action disable this while retaining
    /// `run_speed` as harmless authoring data.
    pub run_enabled: bool,
    /// Turn speed per display frame.
    pub yaw_step: Angle,
    /// Downward acceleration in Q8 engine units per fixed 60 Hz tick squared.
    pub gravity_per_tick_q8: i32,
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
    /// Roll travel speed in Q8 world units per display frame.
    pub roll_speed: i32,
    /// Redistribute roll travel into a short launch and decelerating tail.
    /// Total unobstructed distance stays speed * active frames.
    pub roll_decelerates: bool,
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
    /// Legacy quickstep travel speed in Q8 world units per display frame.
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
            run_enabled: true,
            yaw_step,
            gravity_per_tick_q8: GRAVITY_PER_TICK * 256,
            weight_q8: DEFAULT_WEIGHT_Q8,
            stamina_max_q12: DEFAULT_STAMINA_MAX_Q12,
            sprint_min_q12: 384,
            sprint_drain_q12: 40,
            stamina_recover_q12: 36,
            roll_cost_q12: 768,
            roll_speed: 6 << 8,
            roll_decelerates: false,
            roll_active_frames: 14,
            roll_recovery_frames: 12,
            roll_invulnerable_frames: 10,
            backstep_cost_q12: 512,
            backstep_speed: 5 << 8,
            backstep_active_frames: 8,
            backstep_recovery_frames: 10,
            backstep_invulnerable_frames: 6,
        }
    }

    /// Disable every stamina gate and cost while preserving the authored
    /// maximum for compatibility with UI bindings and saved character data.
    ///
    /// A motor using this profile can sprint and evade indefinitely because
    /// its stamina value never decreases.
    pub const fn without_stamina_limit(mut self) -> Self {
        self.sprint_min_q12 = 0;
        self.sprint_drain_q12 = 0;
        self.roll_cost_q12 = 0;
        self.backstep_cost_q12 = 0;
        self
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

impl CharacterMotorInput {
    /// Keep analog direction but use one locomotion pace outside the caller's
    /// deadzone. Apply before animation startup ramps, so they still accelerate.
    pub fn with_full_move_intent(mut self) -> Self {
        let x = self.move_x.raw();
        let z = self.move_z.raw();
        let magnitude =
            isqrt_i32(square_i32_saturating(x).saturating_add(square_i32_saturating(z)));
        if magnitude > 0 {
            self.move_x = Q12::ONE.mul_ratio(x, magnitude);
            self.move_z = Q12::ONE.mul_ratio(z, magnitude);
        }
        self
    }
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
    /// Vertical velocity in Q8 engine units per tick (negative = falling).
    /// Non-zero only while the body is airborne over a ledge or hole;
    /// reset to zero on landing.
    velocity_y_q8: i32,
    /// Fractional downward displacement retained between fixed ticks.
    remainder_y_q8: i32,
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
    /// Sub-unit XZ displacement carried between ticks (Q8). Speeds are Q8
    /// units per tick, so a walk below one unit per tick moves the integer
    /// position only when the remainder rolls over a whole unit.
    remainder_x_q8: i32,
    remainder_z_q8: i32,
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
            velocity_y_q8: 0,
            remainder_y_q8: 0,
            grounded: false,
            ground_floor: 0,
            ground_anchor_x: 0,
            ground_anchor_z: 0,
            remainder_x_q8: 0,
            remainder_z_q8: 0,
        }
    }

    /// Fold a Q8 displacement into the remainders and return the whole
    /// units to move this tick.
    fn take_whole_units(&mut self, dx_q8: i32, dz_q8: i32) -> (i32, i32) {
        self.remainder_x_q8 = self.remainder_x_q8.saturating_add(dx_q8);
        self.remainder_z_q8 = self.remainder_z_q8.saturating_add(dz_q8);
        let dx = self.remainder_x_q8 >> 8;
        let dz = self.remainder_z_q8 >> 8;
        self.remainder_x_q8 -= dx << 8;
        self.remainder_z_q8 -= dz << 8;
        (dx, dz)
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
        self.velocity_y_q8 = 0;
        self.remainder_y_q8 = 0;
        self.grounded = false;
        self.remainder_x_q8 = 0;
        self.remainder_z_q8 = 0;
    }

    /// Place a traversal body without refunding stamina or retaining fall speed.
    pub fn teleport_to(&mut self, position: RoomPoint) {
        let stamina = self.stamina_q12;
        self.snap_to(position, self.yaw);
        self.stamina_q12 = stamina;
    }

    /// Whether the last motor solve found supporting ground.
    pub const fn grounded(&self) -> bool {
        self.grounded
    }

    /// Signed airborne speed in Q8 world units per fixed tick.
    pub const fn vertical_speed_q8(&self) -> i32 {
        self.velocity_y_q8
    }

    /// Hold a tethered body without running gravity, locomotion or stamina recovery.
    /// Releasing the tether resumes ordinary gravity from rest on the next update.
    pub fn suspended_frame(&mut self, facing: Option<Angle>) -> CharacterMotorFrame {
        if let Some(yaw) = facing {
            self.face(yaw);
        }
        self.velocity_y_q8 = 0;
        self.remainder_y_q8 = 0;
        self.grounded = false;
        self.frame(
            CharacterMotorAnim::Idle,
            CharacterMotorAction::Idle,
            false,
            false,
            false,
            false,
            false,
        )
    }

    /// Turn the body to `yaw` at once. Position, stamina and any in-progress
    /// action are untouched; a lock-on attack uses this to square up to its
    /// target on its first frame.
    pub fn face(&mut self, yaw: Angle) {
        self.yaw = yaw;
    }

    /// Interrupt a fixed action without refunding stamina or changing gravity.
    pub fn interrupt_action(&mut self) {
        self.action = CharacterMotorAction::Idle;
        self.action_frame = 0;
        self.action_yaw = self.yaw;
        self.sprint_latched = false;
        self.remainder_x_q8 = 0;
        self.remainder_z_q8 = 0;
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

        if let Some((move_x, move_z, move_mag)) = analog_move_vector(input) {
            let move_yaw = yaw_from_vector(move_x, move_z);
            // Lock-on constrains ordinary locomotion to face the target, but
            // sprinting deliberately breaks that facing constraint: the body
            // turns into the requested direction and plays the real run. The
            // target itself remains locked, so the next non-sprint frame
            // turns the actor back toward it.
            let wants_sprint = input.sprint;
            self.update_sprint_gate(wants_sprint);
            let sprinting = self.can_sprint(wants_sprint, config);
            let run_freely = sprinting && input.facing_yaw.is_some();
            let (move_x, move_z, directional_anim, locked_lateral) = if run_freely {
                // The locked walk may have the actor facing anywhere relative
                // to this vector. Snap at the gait change so the run starts in
                // the direction the player requested instead of arcing toward
                // it while covering ground in the wrong direction.
                self.yaw = move_yaw;
                (
                    self.yaw.sin().mul_q12(move_mag),
                    self.yaw.cos().mul_q12(move_mag),
                    CharacterMotorAnim::Run,
                    false,
                )
            } else if let Some(facing_yaw) = input.facing_yaw {
                self.yaw = self.yaw.approach_q12(facing_yaw, config.yaw_step.as_q12());
                let directional_anim = locked_locomotion_anim(facing_yaw, move_yaw);
                (
                    move_x,
                    move_z,
                    directional_anim,
                    !matches!(directional_anim, CharacterMotorAnim::Walk),
                )
            } else {
                // Free movement turns at the character's turn rate and
                // travels along the facing, so a stick reversal is a tight
                // U-turn instead of a snap; the feet always point along
                // the path.
                self.yaw = self.yaw.approach_q12(move_yaw, config.yaw_step.as_q12());
                (
                    self.yaw.sin().mul_q12(move_mag),
                    self.yaw.cos().mul_q12(move_mag),
                    CharacterMotorAnim::Walk,
                    false,
                )
            };
            let base_speed = if sprinting {
                config.run_speed
            } else if locked_lateral {
                // Souls-style: strafing and backing off are slower than
                // walking forward, and the backward/strafe clips stride
                // shorter, so the feet track the ground.
                let percent = match input
                    .facing_yaw
                    .map(|facing_yaw| locked_locomotion_anim(facing_yaw, move_yaw))
                {
                    Some(CharacterMotorAnim::WalkBackward) => LOCKED_BACKWARD_SPEED_PERCENT,
                    _ => LOCKED_STRAFE_SPEED_PERCENT,
                };
                config.walk_speed.saturating_mul(percent) / 100
            } else {
                config.walk_speed
            };
            let speed = move_mag.mul_i32(base_speed);
            let directional_anim = if sprinting {
                CharacterMotorAnim::Run
            } else {
                directional_anim
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

            // `directional_anim` already resolves the speed: `Run` whenever
            // sprinting, and the walk-speed direction otherwise.
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

        if let Some(facing_yaw) = input.facing_yaw {
            self.yaw = self.yaw.approach_q12(facing_yaw, config.yaw_step.as_q12());
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
            let speed = if config.roll_decelerates && self.action == CharacterMotorAction::Roll {
                decelerating_roll_step(profile.speed, frame, profile.active_frames)
            } else {
                profile.speed
            };
            let signed_speed = speed.saturating_mul(profile.direction as i32);
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
        // Q12 direction * Q8 speed >> 12 = Q8 units this tick.
        let (dx, dz) = self.take_whole_units(
            yaw.sin().mul_i32(signed_speed),
            yaw.cos().mul_i32(signed_speed),
        );
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
        let (dx, dz) = self.take_whole_units(move_x.mul_i32(speed), move_z.mul_i32(speed));
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

    /// Adopt a committed step's own floor measurement as the ground cache, so
    /// the next tick's [`Self::apply_vertical`] takes its grounded fast path
    /// instead of re-tracing the floor the step just traced.
    ///
    /// Guarded so the reuse is exact rather than merely plausible. The step's
    /// floor probe swept down from `probe_start_y + TRACE_FLOOR_PROBE_LIFT`
    /// and contacted `floor`, which proves the hull is clear everywhere above
    /// `floor` up to that height. When `floor <= probe_start_y`, the next
    /// tick's probe starts at `floor + LIFT`, inside that proven-clear span,
    /// so it contacts the same plane at the same height: `plane_contact`
    /// re-solves against the query segment and a horizontal contact is
    /// independent of where the segment started. A step UP starts the next
    /// probe above anything this one examined, so it is not adopted.
    ///
    /// `self.grounded` is never changed here, only the cached floor and its
    /// anchor, so an airborne body keeps falling exactly as before.
    fn adopt_step_floor(&mut self, grounded_floor: Option<i32>, probe_start_y: i32) {
        if let Some(floor) = grounded_floor {
            if self.grounded && floor <= probe_start_y {
                self.set_grounded(floor);
            }
        }
    }

    fn try_commit_move<C: CharacterCollisionBackend>(
        &mut self,
        collision: &mut C,
        target: RoomPoint,
        radius: i32,
        height: i32,
    ) -> Result<(bool, bool), CollisionQueryError> {
        let shape = CollisionTraceShape::Body { radius, height };
        let probe_start_y = self.position.y;
        if let Some(stand) = collision.move_position(self.position, target, shape, self.grounded)? {
            self.position = stand.position;
            self.adopt_step_floor(stand.grounded_floor, probe_start_y);
            return Ok((true, false));
        }

        let start = self.position;
        let x_only = RoomPoint::new(target.x, start.y, start.z);
        if let Some(stand) = collision.move_position(start, x_only, shape, self.grounded)? {
            let position = stand.position;
            self.position = position;
            self.adopt_step_floor(stand.grounded_floor, probe_start_y);
            return Ok((position.x != start.x || position.z != start.z, true));
        }

        let z_only = RoomPoint::new(start.x, start.y, target.z);
        if let Some(stand) = collision.move_position(start, z_only, shape, self.grounded)? {
            let position = stand.position;
            self.position = position;
            self.adopt_step_floor(stand.grounded_floor, probe_start_y);
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
        // This branch is only reachable while airborne (the grounded case
        // returned above), so the recovery solve's floor is deliberately not
        // adopted: `adopt_step_floor` would reject it anyway.
        let Some(stand) = collision.recovery_position(start, shape)? else {
            return Ok((false, true));
        };
        self.position = stand.position;
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
        if !wants_sprint || !config.run_enabled {
            self.sprint_latched = false;
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
            self.velocity_y_q8 = 0;
            self.remainder_y_q8 = 0;
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
            self.velocity_y_q8 = 0;
            self.remainder_y_q8 = 0;
            return Ok(());
        };

        if self.position.y <= floor
            || (self.grounded && self.position.y.saturating_sub(floor) <= STEP_DOWN_HEIGHT)
        {
            // On the floor, or within a step of it: snap down and ground.
            // Caching the cell lets the next idle tick take the fast path.
            self.position.y = floor;
            self.velocity_y_q8 = 0;
            self.remainder_y_q8 = 0;
            self.set_grounded(floor);
            return Ok(());
        }

        // Airborne: the floor is more than a step below (a ledge or hole).
        // Accelerate downward (clamped to terminal) and move, landing exactly
        // on the floor without overshooting through it.
        self.grounded = false;
        let gravity = config
            .gravity_per_tick_q8
            .saturating_mul(config.weight_q8 as i32)
            / DEFAULT_WEIGHT_Q8 as i32;
        self.velocity_y_q8 = self
            .velocity_y_q8
            .saturating_sub(gravity)
            .max(-MAX_FALL_SPEED * 256);
        self.remainder_y_q8 = self.remainder_y_q8.saturating_add(self.velocity_y_q8);
        let dy = self.remainder_y_q8 / 256;
        self.remainder_y_q8 -= dy * 256;
        let next = self.position.y.saturating_add(dy);
        if next <= floor {
            self.position.y = floor;
            self.velocity_y_q8 = 0;
            self.remainder_y_q8 = 0;
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

/// Approximation of observed quickstep travel: 64% by 0.367s, 80% by
/// 0.5s and 90% by 0.633s for a 60-tick travel interval. Cumulative
/// differences preserve total Q8 distance regardless of interval length.
fn decelerating_roll_step(speed: i32, frame: u8, active_frames: u8) -> i32 {
    let cumulative = |tick: u16| -> i32 {
        let phase = i32::from(tick) * 60 * 256 / i32::from(active_frames.max(1));
        let knots = [
            (0, 0),
            (4, 350),
            (12, 3200),
            (22, 6400),
            (30, 8000),
            (38, 9000),
            (60, 10000),
        ];
        let mut fraction = 10000;
        for pair in knots.windows(2) {
            let (a, va) = pair[0];
            let (b, vb) = pair[1];
            if phase <= b * 256 {
                fraction = va + (vb - va) * (phase - a * 256).max(0) / ((b - a) * 256);
                break;
            }
        }
        psx_math::int32::mul_div_i32(
            speed.max(0).saturating_mul(i32::from(active_frames)),
            fraction,
            10000,
        )
    };
    (cumulative(u16::from(frame) + 1) - cumulative(u16::from(frame))).max(0)
}

fn normalize_config(mut config: CharacterMotorConfig) -> CharacterMotorConfig {
    config.radius = config.radius.max(0);
    config.height = config.height.max(1);
    config.walk_speed = config.walk_speed.max(0);
    config.run_speed = config.run_speed.max(config.walk_speed);
    if config.yaw_step == Angle::ZERO {
        config.yaw_step = Angle::from_q12(1);
    }
    config.gravity_per_tick_q8 = config.gravity_per_tick_q8.max(0);
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
    let near_end = position.with_y(position.y.saturating_sub(TRACE_FLOOR_NEAR_PROBE_DOWN));
    let near = trace_collision(
        provider,
        CollisionTraceQuery {
            start,
            end: near_end,
            shape,
        },
    )?;
    if near.start_solid || near.all_solid {
        return Ok(None);
    }
    if near.fraction_q12 < COLLISION_FRACTION_ONE_Q12 {
        if near.normal_q12[1] < MIN_WALKABLE_FLOOR_NORMAL_Y_Q12 {
            return Ok(None);
        }
        return Ok(Some(near.end.y));
    }

    let far_end = position.with_y(position.y.saturating_sub(TRACE_FLOOR_PROBE_DOWN));
    let far = trace_collision(
        provider,
        CollisionTraceQuery {
            start,
            end: far_end,
            shape,
        },
    )?;
    if far.start_solid
        || far.all_solid
        || far.fraction_q12 >= COLLISION_FRACTION_ONE_Q12
        || far.normal_q12[1] < MIN_WALKABLE_FLOOR_NORMAL_Y_Q12
    {
        return Ok(None);
    }
    Ok(Some(far.end.y))
}

fn trace_stand_position<P: CollisionTraceProvider + ?Sized>(
    provider: &mut P,
    start: RoomPoint,
    target: RoomPoint,
    shape: CollisionTraceShape,
) -> Result<Option<StandOutcome>, CollisionQueryError> {
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
        let settled_y = resolve_step_down(target.y, floor);
        return Ok(Some(StandOutcome {
            position: target.with_y(settled_y),
            // A deeper drop than one step keeps the feet where they were and
            // leaves the body over a ledge: that is not standing on `floor`.
            grounded_floor: (settled_y == floor).then_some(floor),
        }));
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
    // A body exactly on an expanded slope can be classified start-solid by
    // the plane epsilon even though the complete upward sweep exits into
    // empty space. Accept that full escape; an all-solid or partial lift is
    // still a real ceiling/wall obstruction and must reject the step.
    if lift.all_solid || lift.fraction_q12 < COLLISION_FRACTION_ONE_Q12 {
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
        || settle.normal_q12[1] < MIN_WALKABLE_FLOOR_NORMAL_Y_Q12
    {
        return Ok(None);
    }
    // The settle sweep ended on an upward-facing floor, so the step-up lands
    // standing exactly on it.
    Ok(Some(StandOutcome {
        position: settle.end,
        grounded_floor: Some(settle.end.y),
    }))
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

/// Collision-check one exact non-player movement direction through a trace
/// provider. Unlike [`commit_body_step_with_trace_provider`], this does not
/// retry X-only and Z-only slides after a blocked diagonal. The entity heading
/// search already probes the cardinal headings as candidates of their own, so
/// a slide cascade inside every candidate would only repeat hull traces.
pub fn commit_body_direction_with_trace_provider<P: CollisionTraceProvider + ?Sized>(
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
    match trace_body_grounded_stand_position(provider, start, target, shape)? {
        Some(position) => Ok(BodyStep {
            position,
            moved: true,
            blocked: false,
        }),
        None => Ok(BodyStep {
            position: start,
            moved: false,
            blocked: true,
        }),
    }
}

fn trace_body_grounded_stand_position<P: CollisionTraceProvider + ?Sized>(
    provider: &mut P,
    start: RoomPoint,
    target: RoomPoint,
    shape: CollisionTraceShape,
) -> Result<Option<RoomPoint>, CollisionQueryError> {
    let Some(stand) = trace_stand_position(provider, start, target, shape)? else {
        return Ok(None);
    };
    let position = stand.position;
    // The solve already traced the floor here and proved the body ends the
    // step standing on it, so the grounding rule below is satisfied by
    // construction (`position.y == floor`, a zero drop) and `with_y(floor)`
    // is `position`. Re-probing would be a second full floor trace per
    // candidate direction on every AI body step. The `floor <= target.y`
    // guard is the same one `adopt_step_floor` uses: a step UP would start
    // the second probe above anything the first one examined.
    if let Some(floor) = stand.grounded_floor {
        if floor <= target.y {
            return Ok(Some(position));
        }
    }
    let Some(floor) = trace_supporting_floor(provider, position, shape)? else {
        return Ok(None);
    };
    if position.y.saturating_sub(floor) > STEP_DOWN_HEIGHT {
        return Ok(None);
    }
    Ok(Some(position.with_y(floor)))
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

/// Locked-on backward walk speed as a percentage of `walk_speed`.
/// ponytail: constants; promote to CharacterMotorConfig when a project
/// wants to author them.
pub const LOCKED_BACKWARD_SPEED_PERCENT: i32 = 40;
/// Locked-on strafe speed as a percentage of `walk_speed`.
pub const LOCKED_STRAFE_SPEED_PERCENT: i32 = 90;

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
    } else if delta > 0 {
        CharacterMotorAnim::DashLeft
    } else {
        CharacterMotorAnim::DashRight
    }
}

/// Yaw runs from +Z toward +X (`x = sin`, `z = cos`); in the engine's
/// right-handed Y-up frame that is a turn to the LEFT, so a positive delta
/// from facing to move direction means the move is on the left side. (The
/// old `delta < 0 => Left` was a leftover of the mirrored brush world.)
fn locked_locomotion_anim(facing_yaw: Angle, move_yaw: Angle) -> CharacterMotorAnim {
    let delta = facing_yaw.shortest_delta_q12(move_yaw);
    let abs_delta = i32::from(delta).abs();
    if abs_delta <= 512 {
        CharacterMotorAnim::Walk
    } else if abs_delta >= 1536 {
        CharacterMotorAnim::WalkBackward
    } else if delta > 0 {
        CharacterMotorAnim::StrafeLeft
    } else {
        CharacterMotorAnim::StrafeRight
    }
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

/// Trace one point or upright moving body against one static prop AABB.
///
/// Point traces use a deterministic three-axis slab entry for camera and
/// gameplay visibility. Body blockers participate only in horizontal sweeps
/// and zero-length recovery probes. They are not supporting BSP floors, and
/// the bounded step lift must not make a low prop silently steppable (the grid
/// backend has always treated these authored blockers as obstacles). Body
/// candidate fractions cover every AABB side transition and every rounded
/// cylinder/AABB corner; the final entry is refined at Q0.12 precision.
fn trace_aabb_blocker(
    query: CollisionTraceQuery,
    blocker: CharacterCollisionAabb,
) -> Option<CollisionTrace> {
    match query.shape {
        CollisionTraceShape::Point => trace_point_aabb_blocker(query, blocker),
        CollisionTraceShape::Body { radius, height } => {
            trace_body_aabb_blocker(query, blocker, radius, height)
        }
    }
}

/// Trace an integer point segment through a canonical finite-volume AABB.
///
/// Axis order X, Y, Z is the stable tie break when a segment enters an edge or
/// corner. Boundary starts moving inward contact at fraction zero; tangent or
/// outward motion remains clear. Bounded binary searches find exact occupied
/// Q0.12 samples. If a thin continuous crossing falls between samples, floored
/// 32-bit slab intervals conservatively block its containing fraction bin.
fn trace_point_aabb_blocker(
    query: CollisionTraceQuery,
    blocker: CharacterCollisionAabb,
) -> Option<CollisionTrace> {
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

    let movement = [
        query.end.x.saturating_sub(query.start.x),
        query.end.y.saturating_sub(query.start.y),
        query.end.z.saturating_sub(query.start.z),
    ];
    if point_strictly_inside_aabb(query.start, min, max) {
        return Some(CollisionTrace {
            all_solid: point_inside_aabb(query.end, min, max),
            start_solid: true,
            fraction_q12: 0,
            end: query.start,
            normal_q12: point_inside_aabb_normal(query.start, min, max),
            plane_distance: 0,
        });
    }
    if point_inside_aabb(query.start, min, max) {
        let normal_q12 = point_boundary_entry_normal(query.start, min, max, movement)?;
        return Some(CollisionTrace {
            all_solid: false,
            start_solid: false,
            fraction_q12: 0,
            end: query.start,
            normal_q12,
            plane_distance: 0,
        });
    }

    debug_assert!(point_aabb_query_is_representable(query));
    let starts = [query.start.x, query.start.y, query.start.z];
    let ends = [query.end.x, query.end.y, query.end.z];
    let mins = [min.x, min.y, min.z];
    let maxs = [max.x, max.y, max.z];
    let negative_normals = [[-4096, 0, 0], [0, -4096, 0], [0, 0, -4096]];
    let positive_normals = [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
    let mut enter_q12 = 0i32;
    let mut exit_q12 = COLLISION_FRACTION_ONE_Q12;
    let mut entry_normal = None;
    let mut axis = 0usize;
    while axis < 3 {
        let Some((axis_enter, axis_exit)) =
            point_axis_admissible_interval_q12(starts[axis], ends[axis], mins[axis], maxs[axis])
        else {
            return trace_point_aabb_continuous_fallback(query, min, max, movement);
        };
        let normal = if movement[axis] > 0 {
            negative_normals[axis]
        } else {
            positive_normals[axis]
        };
        if axis_enter > 0
            && (axis_enter > enter_q12 || (axis_enter == enter_q12 && entry_normal.is_none()))
        {
            enter_q12 = axis_enter;
            entry_normal = Some(normal);
        }
        exit_q12 = exit_q12.min(axis_exit);
        if enter_q12 > exit_q12 {
            return trace_point_aabb_continuous_fallback(query, min, max, movement);
        }
        axis += 1;
    }
    let fraction_q12 = enter_q12.min(COLLISION_FRACTION_ONE_Q12 - 1);
    debug_assert!(point_inside_aabb(
        trace_lerp_point(query.start, query.end, enter_q12),
        min,
        max
    ));
    Some(CollisionTrace {
        all_solid: false,
        start_solid: false,
        fraction_q12,
        end: trace_lerp_point(query.start, query.end, fraction_q12),
        normal_q12: entry_normal
            .unwrap_or_else(|| point_entry_normal_from_outside(query.start, min, max, movement)),
        plane_distance: 0,
    })
}

/// Whether every scalar multiplication used by the point interpolator is
/// exactly representable. The provider reports an output-preserving failure
/// when this is false, so arithmetic saturation can never turn a prop crossing
/// into a clear trace.
fn point_aabb_query_is_representable(query: CollisionTraceQuery) -> bool {
    if !matches!(query.shape, CollisionTraceShape::Point) {
        return true;
    }
    [
        query.end.x.saturating_sub(query.start.x),
        query.end.y.saturating_sub(query.start.y),
        query.end.z.saturating_sub(query.start.z),
    ]
    .into_iter()
    .all(|delta| delta.checked_mul(COLLISION_FRACTION_ONE_Q12).is_some())
}

/// Exact inclusive Q0.12 sample interval whose scalar interpolation lies in
/// one AABB slab. The scalar operation matches `trace_lerp_point`, including
/// signed floor and saturating arithmetic. Both binary searches are bounded by
/// the 4,097-value fraction domain.
fn point_axis_admissible_interval_q12(
    start: i32,
    end: i32,
    min: i32,
    max: i32,
) -> Option<(i32, i32)> {
    let delta = end.saturating_sub(start);
    if delta == 0 {
        return (start >= min && start <= max).then_some((0, COLLISION_FRACTION_ONE_Q12));
    }
    let (entry, first_past) = if delta > 0 {
        (
            first_point_axis_fraction_q12(start, end, |value| value >= min)?,
            first_point_axis_fraction_q12(start, end, |value| value > max),
        )
    } else {
        (
            first_point_axis_fraction_q12(start, end, |value| value <= max)?,
            first_point_axis_fraction_q12(start, end, |value| value < min),
        )
    };
    let exit = first_past
        .map(|fraction| fraction.saturating_sub(1))
        .unwrap_or(COLLISION_FRACTION_ONE_Q12);
    if entry > exit {
        None
    } else {
        Some((entry, exit))
    }
}

fn first_point_axis_fraction_q12(
    start: i32,
    end: i32,
    mut predicate: impl FnMut(i32) -> bool,
) -> Option<i32> {
    if !predicate(point_axis_at_fraction_q12(
        start,
        end,
        COLLISION_FRACTION_ONE_Q12,
    )) {
        return None;
    }
    let mut low = 0i32;
    let mut high = COLLISION_FRACTION_ONE_Q12;
    while low < high {
        let middle = low.saturating_add(high) / 2;
        if predicate(point_axis_at_fraction_q12(start, end, middle)) {
            high = middle;
        } else {
            low = middle.saturating_add(1);
        }
    }
    Some(low)
}

fn point_axis_at_fraction_q12(start: i32, end: i32, fraction_q12: i32) -> i32 {
    start.saturating_add(
        end.saturating_sub(start)
            .saturating_mul(fraction_q12.clamp(0, COLLISION_FRACTION_ONE_Q12))
            >> Q12::FRACTIONAL_BITS,
    )
}

/// Conservative cold path for a real segment/AABB crossing that falls wholly
/// between adjacent Q0.12 samples. Each positive segment parameter is floored
/// into Q0.12. Real overlaps therefore retain an overlapping fraction bin; a
/// near miss narrower than one bin can conservatively block. The returned
/// sample is the floor of continuous entry and remains on the clear side.
fn trace_point_aabb_continuous_fallback(
    query: CollisionTraceQuery,
    min: RoomPoint,
    max: RoomPoint,
    movement: [i32; 3],
) -> Option<CollisionTrace> {
    let starts = [query.start.x, query.start.y, query.start.z];
    let mins = [min.x, min.y, min.z];
    let maxs = [max.x, max.y, max.z];
    let negative_normals = [[-4096, 0, 0], [0, -4096, 0], [0, 0, -4096]];
    let positive_normals = [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
    let mut enter_q12 = 0i32;
    let mut exit_q12 = COLLISION_FRACTION_ONE_Q12;
    let mut entry_normal = None;
    let mut axis = 0usize;
    while axis < 3 {
        let start = starts[axis];
        let delta = movement[axis];
        if delta == 0 {
            if start < mins[axis] || start > maxs[axis] {
                return None;
            }
            axis += 1;
            continue;
        }
        let (near_distance, far_distance, denominator, normal) = if delta > 0 {
            (
                mins[axis].saturating_sub(start),
                maxs[axis].saturating_sub(start),
                delta,
                negative_normals[axis],
            )
        } else {
            (
                start.saturating_sub(maxs[axis]),
                start.saturating_sub(mins[axis]),
                delta.saturating_neg(),
                positive_normals[axis],
            )
        };
        if far_distance < 0 || near_distance > denominator {
            return None;
        }
        let clipped_near = near_distance.clamp(0, denominator);
        let clipped_far = far_distance.clamp(0, denominator);
        let axis_enter_q12 = div_q12_i32(clipped_near, denominator);
        let axis_exit_q12 = div_q12_i32(clipped_far, denominator);
        if clipped_near > 0
            && (axis_enter_q12 > enter_q12
                || (axis_enter_q12 == enter_q12 && entry_normal.is_none()))
        {
            entry_normal = Some(normal);
        }
        enter_q12 = enter_q12.max(axis_enter_q12);
        exit_q12 = exit_q12.min(axis_exit_q12);
        if enter_q12 > exit_q12 {
            return None;
        }
        axis += 1;
    }
    let fraction_q12 = enter_q12.clamp(0, COLLISION_FRACTION_ONE_Q12 - 1);
    Some(CollisionTrace {
        all_solid: false,
        start_solid: false,
        fraction_q12,
        end: trace_lerp_point(query.start, query.end, fraction_q12),
        normal_q12: entry_normal
            .unwrap_or_else(|| point_entry_normal_from_outside(query.start, min, max, movement)),
        plane_distance: 0,
    })
}

fn point_entry_normal_from_outside(
    start: RoomPoint,
    min: RoomPoint,
    max: RoomPoint,
    movement: [i32; 3],
) -> [i16; 3] {
    let starts = [start.x, start.y, start.z];
    let mins = [min.x, min.y, min.z];
    let maxs = [max.x, max.y, max.z];
    let negative_normals = [[-4096, 0, 0], [0, -4096, 0], [0, 0, -4096]];
    let positive_normals = [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
    let mut axis = 0usize;
    while axis < 3 {
        if starts[axis] < mins[axis] && movement[axis] > 0 {
            return negative_normals[axis];
        }
        if starts[axis] > maxs[axis] && movement[axis] < 0 {
            return positive_normals[axis];
        }
        axis += 1;
    }
    [4096, 0, 0]
}

fn point_inside_aabb(point: RoomPoint, min: RoomPoint, max: RoomPoint) -> bool {
    point.x >= min.x
        && point.x <= max.x
        && point.y >= min.y
        && point.y <= max.y
        && point.z >= min.z
        && point.z <= max.z
}

fn point_strictly_inside_aabb(point: RoomPoint, min: RoomPoint, max: RoomPoint) -> bool {
    point.x > min.x
        && point.x < max.x
        && point.y > min.y
        && point.y < max.y
        && point.z > min.z
        && point.z < max.z
}

fn point_boundary_entry_normal(
    point: RoomPoint,
    min: RoomPoint,
    max: RoomPoint,
    movement: [i32; 3],
) -> Option<[i16; 3]> {
    let points = [point.x, point.y, point.z];
    let mins = [min.x, min.y, min.z];
    let maxs = [max.x, max.y, max.z];
    let negative_normals = [[-4096, 0, 0], [0, -4096, 0], [0, 0, -4096]];
    let positive_normals = [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
    let mut normal = None;
    let mut axis = 0usize;
    while axis < 3 {
        if points[axis] == mins[axis] {
            if movement[axis] <= 0 {
                return None;
            }
            normal.get_or_insert(negative_normals[axis]);
        } else if points[axis] == maxs[axis] {
            if movement[axis] >= 0 {
                return None;
            }
            normal.get_or_insert(positive_normals[axis]);
        }
        axis += 1;
    }
    normal
}

fn point_inside_aabb_normal(point: RoomPoint, min: RoomPoint, max: RoomPoint) -> [i16; 3] {
    let faces = [
        (point.x.saturating_sub(min.x), [-4096, 0, 0]),
        (max.x.saturating_sub(point.x), [4096, 0, 0]),
        (point.y.saturating_sub(min.y), [0, -4096, 0]),
        (max.y.saturating_sub(point.y), [0, 4096, 0]),
        (point.z.saturating_sub(min.z), [0, 0, -4096]),
        (max.z.saturating_sub(point.z), [0, 0, 4096]),
    ];
    let mut best = faces[0];
    for face in faces.into_iter().skip(1) {
        if face.0 < best.0 {
            best = face;
        }
    }
    best.1
}

fn trace_body_aabb_blocker(
    query: CollisionTraceQuery,
    blocker: CharacterCollisionAabb,
    radius: i32,
    height: i32,
) -> Option<CollisionTrace> {
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
    let point_x_q8 = query.start.x.saturating_mul(256).saturating_add(
        query
            .end
            .x
            .saturating_sub(query.start.x)
            .saturating_mul(fraction_q12)
            >> 4,
    );
    let point_z_q8 = query.start.z.saturating_mul(256).saturating_add(
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

#[cfg(test)]
mod tests {
    #[test]
    fn hook_teleport_clears_fall_speed_without_refunding_stamina() {
        let mut motor =
            super::CharacterMotorState::new(super::RoomPoint::ZERO, super::Angle::QUARTER);
        motor.stamina_q12 = 1234;
        motor.velocity_y_q8 = -40;
        motor.action = super::CharacterMotorAction::Roll;
        motor.teleport_to(super::RoomPoint::new(4, 200, 8));
        assert_eq!(motor.stamina_q12, 1234);
        assert_eq!(motor.velocity_y_q8, 0);
        assert!(motor.action.is_idle());
        assert_eq!(motor.position, super::RoomPoint::new(4, 200, 8));
        assert_eq!(motor.yaw, super::Angle::QUARTER);
    }
    #[test]
    fn interruption_keeps_position_stamina_and_falling_velocity() {
        let mut motor =
            super::CharacterMotorState::new(super::RoomPoint::new(1, 2, 3), super::Angle::ZERO);
        motor.action = super::CharacterMotorAction::Roll;
        motor.action_frame = 7;
        motor.stamina_q12 = 1234;
        motor.velocity_y_q8 = -20;
        motor.interrupt_action();
        assert!(motor.action.is_idle());
        assert_eq!(motor.action_frame, 0);
        assert_eq!(motor.stamina_q12, 1234);
        assert_eq!(motor.velocity_y_q8, -20);
        assert_eq!(motor.position, super::RoomPoint::new(1, 2, 3));
    }
    use super::*;

    /// Open flat-floor stand-in for the removed grid backend's "no room"
    /// collision: floor at y = 0, nothing else in the world.
    trait OpenFloorUpdate {
        fn update_vblanks(
            &mut self,
            collision: Option<core::convert::Infallible>,
            input: CharacterMotorInput,
            config: CharacterMotorConfig,
            delta_vblanks: u16,
        ) -> CharacterMotorFrame;

        fn update(
            &mut self,
            collision: Option<core::convert::Infallible>,
            input: CharacterMotorInput,
            config: CharacterMotorConfig,
        ) -> CharacterMotorFrame;
    }

    impl OpenFloorUpdate for CharacterMotorState {
        fn update_vblanks(
            &mut self,
            collision: Option<core::convert::Infallible>,
            input: CharacterMotorInput,
            config: CharacterMotorConfig,
            delta_vblanks: u16,
        ) -> CharacterMotorFrame {
            assert!(collision.is_none());
            self.update_vblanks_with_trace_provider(
                &mut FlatTraceProvider::new(None),
                input,
                config,
                delta_vblanks,
            )
            .expect("flat floor traces never fail")
        }

        fn update(
            &mut self,
            collision: Option<core::convert::Infallible>,
            input: CharacterMotorInput,
            config: CharacterMotorConfig,
        ) -> CharacterMotorFrame {
            self.update_vblanks(collision, input, config, 1)
        }
    }

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

    #[test]
    fn floor_probe_resolves_near_slope_before_long_q12_fallback() {
        struct NearSlopeProvider {
            calls: u8,
        }

        impl CollisionTraceProvider for NearSlopeProvider {
            fn trace_into(
                &mut self,
                query: CollisionTraceQuery,
                output: &mut CollisionTrace,
            ) -> bool {
                self.calls = self.calls.saturating_add(1);
                let span = query.start.y.saturating_sub(query.end.y);
                *output = if span <= TRACE_FLOOR_NEAR_PROBE_DOWN + TRACE_FLOOR_PROBE_LIFT {
                    CollisionTrace {
                        fraction_q12: 10,
                        end: query.start.with_y(1),
                        normal_q12: [0, 3974, -993],
                        ..CollisionTrace::default()
                    }
                } else {
                    // This is the failure mode from the authored arena ramp:
                    // a 32K-unit Q12 sweep quantizes the near slope away and
                    // reports the lower flat floor instead.
                    CollisionTrace {
                        fraction_q12: 0,
                        end: query.start.with_y(0),
                        normal_q12: [0, 4096, 0],
                        ..CollisionTrace::default()
                    }
                };
                true
            }
        }

        let mut provider = NearSlopeProvider { calls: 0 };
        let floor = trace_supporting_floor(
            &mut provider,
            RoomPoint::new(689, 1, 11),
            CollisionTraceShape::Body {
                radius: 12,
                height: 64,
            },
        )
        .expect("floor trace");
        assert_eq!(floor, Some(1));
        assert_eq!(provider.calls, 1, "near contact must skip the long probe");
    }

    #[test]
    fn a_face_steeper_than_the_walk_limit_is_not_floor() {
        struct SlopeProvider {
            normal_y: i16,
        }

        impl CollisionTraceProvider for SlopeProvider {
            fn trace_into(
                &mut self,
                query: CollisionTraceQuery,
                output: &mut CollisionTrace,
            ) -> bool {
                *output = CollisionTrace {
                    fraction_q12: 10,
                    end: query.start.with_y(1),
                    normal_q12: [0, self.normal_y, 0],
                    ..CollisionTrace::default()
                };
                true
            }
        }

        let probe = |normal_y: i16| {
            trace_supporting_floor(
                &mut SlopeProvider { normal_y },
                RoomPoint::new(0, 8, 0),
                CollisionTraceShape::Body {
                    radius: 12,
                    height: 64,
                },
            )
            .expect("floor trace")
        };

        // Flat ground and a gentle ramp are floor.
        assert_eq!(probe(4096), Some(1), "flat ground is floor");
        assert_eq!(probe(3547), Some(1), "a 30 degree ramp is floor");
        assert_eq!(
            probe(MIN_WALKABLE_FLOOR_NORMAL_Y_Q12),
            Some(1),
            "the limit itself is floor"
        );

        // Anything steeper is a wall. Before the limit existed these all
        // reported floor, so a body could walk up an almost sheer brush: the
        // only test was that the upward component be positive at all.
        assert_eq!(
            probe(MIN_WALKABLE_FLOOR_NORMAL_Y_Q12 - 1),
            None,
            "just past the limit is a wall"
        );
        assert_eq!(probe(2048), None, "a 60 degree face is a wall");
        assert_eq!(probe(41), None, "an almost vertical face is a wall");
        assert_eq!(probe(0), None, "a vertical wall is not floor");
    }

    #[test]
    fn floor_probe_falls_back_to_long_trace_after_near_miss() {
        struct DistantFloorProvider {
            calls: u8,
        }

        impl CollisionTraceProvider for DistantFloorProvider {
            fn trace_into(
                &mut self,
                query: CollisionTraceQuery,
                output: &mut CollisionTrace,
            ) -> bool {
                self.calls = self.calls.saturating_add(1);
                *output = if self.calls == 1 {
                    CollisionTrace::unobstructed(query.end)
                } else {
                    CollisionTrace {
                        fraction_q12: 32,
                        end: query.start.with_y(-255),
                        normal_q12: [0, COLLISION_FRACTION_ONE_Q12 as i16, 0],
                        ..CollisionTrace::default()
                    }
                };
                true
            }
        }

        let mut provider = DistantFloorProvider { calls: 0 };
        let floor = trace_supporting_floor(
            &mut provider,
            RoomPoint::new(0, 0, 0),
            CollisionTraceShape::Body {
                radius: 12,
                height: 64,
            },
        )
        .expect("floor trace");
        assert_eq!(floor, Some(-255));
        assert_eq!(
            provider.calls, 2,
            "near miss must retain the ledge fallback"
        );
    }

    #[test]
    fn slope_step_accepts_complete_lift_out_of_floor_epsilon() {
        struct SlopeStepProvider {
            calls: u8,
        }

        impl CollisionTraceProvider for SlopeStepProvider {
            fn trace_into(
                &mut self,
                query: CollisionTraceQuery,
                output: &mut CollisionTrace,
            ) -> bool {
                self.calls = self.calls.saturating_add(1);
                *output = match self.calls {
                    1 => CollisionTrace {
                        all_solid: true,
                        start_solid: true,
                        fraction_q12: COLLISION_FRACTION_ONE_Q12,
                        end: query.end,
                        ..CollisionTrace::default()
                    },
                    2 => CollisionTrace {
                        start_solid: true,
                        fraction_q12: COLLISION_FRACTION_ONE_Q12,
                        end: query.end,
                        ..CollisionTrace::default()
                    },
                    3 => CollisionTrace::unobstructed(query.end),
                    4 => CollisionTrace {
                        fraction_q12: COLLISION_FRACTION_ONE_Q12 / 2,
                        end: query.end.with_y(5),
                        normal_q12: [0, 3974, -993],
                        ..CollisionTrace::default()
                    },
                    _ => panic!("unexpected slope-step trace"),
                };
                true
            }
        }

        let mut provider = SlopeStepProvider { calls: 0 };
        let start = RoomPoint::new(689, 0, 7);
        let target = RoomPoint::new(689, 0, 25);
        let position = trace_stand_position(
            &mut provider,
            start,
            target,
            CollisionTraceShape::Body {
                radius: 12,
                height: 64,
            },
        )
        .expect("slope step trace");
        assert_eq!(
            position,
            Some(StandOutcome {
                position: RoomPoint::new(689, 5, 25),
                // The settle sweep ended on an upward-facing floor, so the
                // step-up reports the floor it landed on.
                grounded_floor: Some(5),
            })
        );
        assert_eq!(provider.calls, 4);
    }

    /// A committed step adopts its own floor measurement, so the next tick's
    /// vertical pass takes the grounded fast path instead of re-tracing the
    /// floor that step just traced. The trace count is the assertion: two
    /// traces on the second tick instead of three, at the identical position.
    #[test]
    fn committed_step_grounds_on_its_own_floor_measurement() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut provider = FlatTraceProvider::new(None);
        let input = CharacterMotorInput {
            walk: 1,
            ..CharacterMotorInput::default()
        };

        // Tick one starts un-grounded, so it pays the vertical floor probe.
        let first = motor
            .update_vblanks_with_trace_provider(&mut provider, input, config_instant_turn(), 1)
            .expect("first step");
        let first_calls = provider.calls;
        assert!(first.moved);
        assert_eq!(motor.position().y, 0, "the step settles on the flat floor");

        provider.calls = 0;
        let second = motor
            .update_vblanks_with_trace_provider(&mut provider, input, config_instant_turn(), 1)
            .expect("second step");
        assert!(second.moved);
        assert_eq!(motor.position().y, 0, "still on the same flat floor");
        assert_eq!(
            provider.calls, 2,
            "the second tick reuses the first step's floor: one body sweep \
             plus one floor probe at the destination, with no third probe \
             re-measuring the floor already under the feet"
        );
        assert!(
            provider.calls < first_calls,
            "the cold first tick must be the one that pays the extra probe"
        );
    }

    /// The adoption is guarded: it is only exact when the measured floor is at
    /// or below the height the probe started from. A step UP starts the next
    /// tick's probe above anything this one examined, so it is not adopted and
    /// the vertical pass re-measures as before.
    #[test]
    fn step_up_does_not_adopt_its_floor_measurement() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        motor.grounded = true;
        motor.set_grounded(0);
        motor.adopt_step_floor(Some(-8), 0);
        assert_eq!(motor.ground_floor, -8, "a step down or level is adopted");

        motor.set_grounded(0);
        motor.adopt_step_floor(Some(12), 0);
        assert_eq!(motor.ground_floor, 0, "a step up is not adopted");

        motor.grounded = false;
        motor.set_grounded(0);
        motor.grounded = false;
        motor.adopt_step_floor(Some(-8), 0);
        assert_eq!(
            motor.ground_floor, 0,
            "an airborne body keeps falling; nothing is adopted"
        );
    }

    fn config() -> CharacterMotorConfig {
        CharacterMotorConfig::character(64, 32 << 8, 64 << 8, Angle::from_q12(16))
    }

    /// Free movement turns toward the stick at `yaw_step` and travels along
    /// the facing; tests that only care about travel use a quarter-turn step
    /// so the first tick already faces the stick.
    fn config_instant_turn() -> CharacterMotorConfig {
        CharacterMotorConfig::character(64, 32 << 8, 64 << 8, Angle::from_q12(1024))
    }

    #[test]
    fn trace_fall_keeps_ballistic_height_while_steering_and_under_catchup() {
        for batch in [1, 3] {
            let mut motor = CharacterMotorState::new(RoomPoint::new(0, 127, 0), Angle::ZERO);
            let mut provider = FlatTraceProvider::new(None);
            let mut cfg = config();
            cfg.gravity_per_tick_q8 = 32;
            cfg.walk_speed = 128;
            for step in 1..=45 / batch {
                let frame = motor
                    .update_vblanks_with_trace_provider(
                        &mut provider,
                        CharacterMotorInput {
                            walk: 1,
                            ..CharacterMotorInput::default()
                        },
                        cfg,
                        batch as u16,
                    )
                    .unwrap();
                let tick = step * batch;
                assert_eq!(
                    frame.position.y,
                    (127 - tick * (tick + 1) / 16).max(0),
                    "tick {tick}"
                );
            }
            assert!(motor.grounded());
            assert_eq!(motor.vertical_speed_q8(), 0);
        }
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
    fn point_aabb_trace_covers_entry_start_inside_miss_and_corner_tie() {
        let blocker =
            CharacterCollisionAabb::new(RoomPoint::new(-2, -2, -2), RoomPoint::new(2, 2, 2));
        let entry = trace_aabb_blocker(
            CollisionTraceQuery::point(RoomPoint::new(-10, 0, 0), RoomPoint::new(10, 0, 0)),
            blocker,
        )
        .expect("point enters AABB");
        assert!(!entry.start_solid);
        assert!(!entry.all_solid);
        assert!((1636..=1640).contains(&entry.fraction_q12), "{entry:?}");
        assert_eq!(entry.end.x, -2);
        assert_eq!(entry.normal_q12, [-4096, 0, 0]);

        let inside = trace_aabb_blocker(
            CollisionTraceQuery::point(RoomPoint::ZERO, RoomPoint::new(1, 1, 1)),
            blocker,
        )
        .expect("point starts inside AABB");
        assert!(inside.start_solid);
        assert!(inside.all_solid);
        assert_eq!(inside.fraction_q12, 0);
        assert_eq!(inside.end, RoomPoint::ZERO);
        assert_eq!(inside.normal_q12, [-4096, 0, 0]);

        assert_eq!(
            trace_aabb_blocker(
                CollisionTraceQuery::point(RoomPoint::new(-10, 8, 0), RoomPoint::new(10, 8, 0)),
                blocker,
            ),
            None,
            "parallel segment outside one slab stays clear"
        );

        let corner = trace_aabb_blocker(
            CollisionTraceQuery::point(RoomPoint::new(-10, -10, 0), RoomPoint::new(10, 10, 0)),
            blocker,
        )
        .expect("point enters an AABB corner");
        assert_eq!(corner.end, RoomPoint::new(-2, -2, 0));
        assert_eq!(
            corner.normal_q12,
            [-4096, 0, 0],
            "X wins an exact X/Y entry tie"
        );
    }

    #[test]
    fn point_aabb_trace_matches_reported_discrete_counterexamples() {
        let blocker =
            CharacterCollisionAabb::new(RoomPoint::new(-2, -2, -2), RoomPoint::new(2, 2, 2));
        let diagonal = trace_aabb_blocker(
            CollisionTraceQuery::point(RoomPoint::new(37, 34, -22), RoomPoint::new(-31, -32, 25)),
            blocker,
        )
        .expect("diagonal enters the box");
        assert_eq!(diagonal.fraction_q12, 2049);
        assert_eq!(diagonal.end, RoomPoint::new(2, 0, 1));
        assert_eq!(diagonal.normal_q12, [4096, 0, 0]);

        let floor = trace_aabb_blocker(
            CollisionTraceQuery::point(RoomPoint::new(0, 3, 0), RoomPoint::new(0, -10_000, 0)),
            blocker,
        )
        .expect("long floor ray enters the box");
        assert_eq!(floor.fraction_q12, 1);
        assert_eq!(floor.end, RoomPoint::new(0, 0, 0));
        assert_eq!(floor.normal_q12, [0, 4096, 0]);
    }

    #[test]
    fn point_axis_intervals_match_a_bounded_discrete_oracle() {
        let mut state = 0x6d2b_79f5u32;
        for case in 0..512 {
            let mut next = || {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as i32
            };
            let start = next().rem_euclid(20_001) - 10_000;
            let end = next().rem_euclid(20_001) - 10_000;
            let a = next().rem_euclid(401) - 200;
            let b = next().rem_euclid(401) - 200;
            let min = a.min(b);
            let max = a.max(b).saturating_add(1);
            let expected_first = (0..=COLLISION_FRACTION_ONE_Q12).find(|&fraction| {
                let value = point_axis_at_fraction_q12(start, end, fraction);
                value >= min && value <= max
            });
            let expected_last = (0..=COLLISION_FRACTION_ONE_Q12).rev().find(|&fraction| {
                let value = point_axis_at_fraction_q12(start, end, fraction);
                value >= min && value <= max
            });
            let expected = expected_first.zip(expected_last);
            assert_eq!(
                point_axis_admissible_interval_q12(start, end, min, max),
                expected,
                "oracle case {case}: start={start} end={end} slab={min}..={max}"
            );
        }
    }

    #[test]
    fn point_aabb_conservatively_blocks_thin_crossings_and_sub_q12_near_misses() {
        let thin = CharacterCollisionAabb::new(RoomPoint::new(2, -1, -1), RoomPoint::new(3, 1, 1));
        let crossed = trace_aabb_blocker(
            CollisionTraceQuery::point(RoomPoint::ZERO, RoomPoint::new(16_384, 0, 0)),
            thin,
        )
        .expect("continuous thin crossing fails closed between samples");
        assert_eq!(crossed.fraction_q12, 0);
        assert_eq!(crossed.normal_q12, [-4096, 0, 0]);

        let near_miss =
            CharacterCollisionAabb::new(RoomPoint::new(1, 2, -1), RoomPoint::new(2, 3, 1));
        let conservative = trace_aabb_blocker(
            CollisionTraceQuery::point(RoomPoint::new(0, 6, 0), RoomPoint::new(16_384, -16_378, 0)),
            near_miss,
        )
        .expect("sub-Q12 disjoint slabs conservatively share one fraction bin");
        assert_eq!(conservative.fraction_q12, 0);
        assert_eq!(conservative.normal_q12, [-4096, 0, 0]);
    }

    #[test]
    fn point_aabb_boundary_motion_is_directional_and_deterministic() {
        let blocker =
            CharacterCollisionAabb::new(RoomPoint::new(-2, -2, -2), RoomPoint::new(2, 2, 2));
        let inward = trace_aabb_blocker(
            CollisionTraceQuery::point(RoomPoint::new(-2, 0, 0), RoomPoint::new(0, 0, 0)),
            blocker,
        )
        .expect("inward boundary motion contacts immediately");
        assert_eq!(inward.fraction_q12, 0);
        assert!(!inward.start_solid);
        assert_eq!(inward.normal_q12, [-4096, 0, 0]);
        assert_eq!(
            trace_aabb_blocker(
                CollisionTraceQuery::point(RoomPoint::new(-2, 0, 0), RoomPoint::new(-4, 0, 0)),
                blocker,
            ),
            None,
            "outward boundary motion is clear"
        );
        assert_eq!(
            trace_aabb_blocker(
                CollisionTraceQuery::point(RoomPoint::new(-2, 0, 0), RoomPoint::new(-2, 1, 0)),
                blocker,
            ),
            None,
            "tangent boundary motion is clear"
        );
    }

    #[test]
    fn collidable_prop_blocks_melee_point_segment_and_world_wins_tie() {
        let query = CollisionTraceQuery::point(RoomPoint::new(-10, 0, 0), RoomPoint::new(10, 0, 0));
        let blocker =
            CharacterCollisionAabb::new(RoomPoint::new(-2, -4, -4), RoomPoint::new(2, 4, 4));
        let prop = trace_aabb_blocker(query, blocker).expect("prop point hit");
        let world_normal = [0, 0, -4096];
        let mut world = FixedTraceProvider {
            trace: CollisionTrace {
                normal_q12: world_normal,
                ..prop
            },
            fail_once: false,
        };
        let props = [blocker];
        let mut provider = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &props);
        let trace = trace_collision(&mut provider, query).expect("compound point trace");
        assert!(trace.hit(), "collidable prop occludes the melee segment");
        assert_eq!(trace.fraction_q12, prop.fraction_q12);
        assert_eq!(trace.normal_q12, world_normal, "world retains exact tie");
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
        let mut sentinel = core::mem::MaybeUninit::<CollisionTrace>::uninit();
        unsafe {
            core::ptr::write_bytes(
                sentinel.as_mut_ptr().cast::<u8>(),
                0xa5,
                core::mem::size_of::<CollisionTrace>(),
            );
            let trace = sentinel.as_mut_ptr();
            core::ptr::addr_of_mut!((*trace).all_solid).write(true);
            core::ptr::addr_of_mut!((*trace).start_solid).write(true);
            core::ptr::addr_of_mut!((*trace).fraction_q12).write(17);
            core::ptr::addr_of_mut!((*trace).end).write(RoomPoint::new(1, 2, 3));
            core::ptr::addr_of_mut!((*trace).normal_q12).write([4, 5, 6]);
            core::ptr::addr_of_mut!((*trace).plane_distance).write(7);
        }
        let trace_bytes = |trace: &CollisionTrace| {
            let mut bytes = [0u8; core::mem::size_of::<CollisionTrace>()];
            unsafe {
                core::ptr::copy_nonoverlapping(
                    core::ptr::from_ref(trace).cast::<u8>(),
                    bytes.as_mut_ptr(),
                    bytes.len(),
                );
            }
            bytes
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
        let output = unsafe { &mut *sentinel.as_mut_ptr() };
        let sentinel_bytes = trace_bytes(output);
        assert!(!provider.trace_into(query, output));
        assert_eq!(trace_bytes(output), sentinel_bytes);

        let point_query = CollisionTraceQuery::point(query.start, query.end);
        assert!(!provider.trace_into(point_query, output));
        assert_eq!(
            trace_bytes(output),
            sentinel_bytes,
            "malformed point-AABB composition preserves every output byte"
        );

        let valid =
            CharacterCollisionAabb::new(RoomPoint::new(10, 0, -4), RoomPoint::new(14, 8, 4));
        let overflow = [valid; psx_level::MAX_STATIC_PROP_AABB_BLOCKERS + 1];
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &overflow);
        assert!(!provider.trace_into(query, output));
        assert_eq!(trace_bytes(output), sentinel_bytes);
        assert!(!provider.trace_into(point_query, output));
        assert_eq!(trace_bytes(output), sentinel_bytes);

        let valid_props = [valid];
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &valid_props);
        let saturating_query = CollisionTraceQuery::point(
            RoomPoint::new(i32::MIN, 0, 0),
            RoomPoint::new(i32::MAX, 0, 0),
        );
        assert!(!provider.trace_into(saturating_query, output));
        assert_eq!(
            trace_bytes(output),
            sentinel_bytes,
            "unrepresentable point interpolation fails without touching bytes"
        );
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
        {
            let mut provider = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], &[]);
            assert_eq!(
                trace_collision(&mut provider, query),
                Ok(CollisionTrace::unobstructed(query.end))
            );
        }
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
    fn trace_body_direction_rejects_blocked_diagonal_without_axis_slide() {
        let blockers = [CharacterCollisionCylinder::new(
            RoomPoint::new(10, 0, 10),
            2,
            8,
        )];
        let mut world = FlatTraceProvider::new(None);
        let mut provider = CharacterBlockerTraceProvider::new(&mut world, &blockers);
        let step =
            commit_body_direction_with_trace_provider(&mut provider, RoomPoint::ZERO, 20, 20, 2, 8)
                .expect("trace body direction");
        assert_eq!(step.position, RoomPoint::ZERO);
        assert!(!step.moved);
        assert!(step.blocked);
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
            config_instant_turn(),
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
            // Facing +Z, right is -X.
            (Q12::ONE, Q12::ZERO, CharacterMotorAnim::DashLeft),
            (Q12::NEG_ONE, Q12::ZERO, CharacterMotorAnim::DashRight),
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
            // Facing +Z, right is -X (right = forward x up).
            (Q12::ONE, Q12::ZERO, CharacterMotorAnim::StrafeLeft),
            (Q12::NEG_ONE, Q12::ZERO, CharacterMotorAnim::StrafeRight),
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
    fn locked_sprint_turns_into_the_movement_direction_and_uses_run() {
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
            let walked = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO).update(
                None,
                input(false),
                config_instant_turn(),
            );
            let sprinted = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO).update(
                None,
                input(true),
                config_instant_turn(),
            );

            // Walking remains target-facing. Sprinting turns into the travel
            // vector and always uses the actual run animation.
            assert_eq!(walked.yaw, Angle::ZERO);
            assert_eq!(sprinted.yaw, yaw_from_vector(move_x, move_z));
            assert!(sprinted.sprinting, "every locked direction should sprint");
            assert_eq!(sprinted.anim, CharacterMotorAnim::Run);
            let walked_distance = walked.position.x.abs() + walked.position.z.abs();
            let sprinted_distance = sprinted.position.x.abs() + sprinted.position.z.abs();
            assert!(
                sprinted_distance > walked_distance,
                "locked sprint must move farther than locked walk ({move_x:?}, {move_z:?})"
            );
        }
    }

    #[test]
    fn locked_lateral_sprint_runs_sideways_then_restores_target_facing() {
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
            config_instant_turn(),
        );

        assert_eq!(frame.yaw, Angle::QUARTER, "run faces its travel direction");
        assert_eq!(frame.anim, CharacterMotorAnim::Run);
        assert!(
            frame.sprinting,
            "sprint stays available sideways under lock"
        );
        assert_eq!(frame.position.x, config().run_speed >> 8);

        let recovered = motor.update(
            None,
            CharacterMotorInput {
                walk: 1,
                move_x: Q12::ONE,
                facing_yaw: Some(Angle::ZERO),
                sprint: false,
                ..CharacterMotorInput::default()
            },
            config_instant_turn(),
        );
        assert_eq!(
            recovered.yaw,
            Angle::ZERO,
            "lock facing resumes after the run"
        );
        assert_eq!(recovered.anim, CharacterMotorAnim::StrafeLeft);
    }

    #[test]
    fn locked_evade_rolls_in_every_direction_without_dropping_lock_input() {
        // Diagonals resolve by the same sectors as locked locomotion:
        // within a quarter-turn of the facing counts as forward, the
        // mirrored sector as backward, the rest as side slides.
        let directions = [
            (Q12::ZERO, Q12::ONE, CharacterMotorAnim::Roll),
            (Q12::ZERO, Q12::NEG_ONE, CharacterMotorAnim::Quickstep),
            // Relative to the +Z lock facing, -X is the right side.
            (Q12::NEG_ONE, Q12::ZERO, CharacterMotorAnim::DashRight),
            (Q12::ONE, Q12::ZERO, CharacterMotorAnim::DashLeft),
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
    fn shorter_evade_keeps_its_direction_and_invulnerability_window() {
        let travel = |speed| {
            let mut config = config();
            config.roll_speed = speed << 8;
            config.roll_active_frames = 22;
            config.roll_recovery_frames = 13;
            config.roll_invulnerable_frames = 10;
            let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
            let mut invulnerable = 0;
            for tick in 0..35 {
                let frame = motor.update(
                    None,
                    CharacterMotorInput {
                        move_x: if tick == 0 { Q12::ONE } else { Q12::NEG_ONE },
                        move_z: Q12::ZERO,
                        facing_yaw: Some(if tick == 0 { Angle::ZERO } else { Angle::HALF }),
                        evade: tick == 0,
                        ..CharacterMotorInput::default()
                    },
                    config,
                );
                invulnerable += usize::from(frame.invulnerable);
                assert_eq!(frame.position.z, 0);
                assert!(
                    frame.position.x >= 0,
                    "later input must not turn the active evade"
                );
            }
            (motor.position().x, invulnerable)
        };
        assert_eq!(travel(191), (191 * 22, 10));
        assert_eq!(travel(112), (112 * 22, 10));
    }

    #[test]
    fn fixed_pace_preserves_direction_and_zero_input() {
        let idle = CharacterMotorInput::default().with_full_move_intent();
        assert_eq!((idle.move_x, idle.move_z), (Q12::ZERO, Q12::ZERO));
        for strength in [64, 512, 2048, 4096] {
            let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
            let input = CharacterMotorInput {
                move_z: Q12::from_raw(strength),
                ..CharacterMotorInput::default()
            }
            .with_full_move_intent();
            for _ in 0..60 {
                motor.update(None, input, config());
            }
            assert_eq!(motor.position().z, config().walk_speed * 60 / 256);
        }
        let diagonal = CharacterMotorInput {
            move_x: Q12::from_raw(300),
            move_z: Q12::from_raw(-400),
            ..CharacterMotorInput::default()
        }
        .with_full_move_intent();
        assert!((diagonal.move_x.raw() * 4 + diagonal.move_z.raw() * 3).abs() <= 4);
        let length = isqrt_i32(
            square_i32_saturating(diagonal.move_x.raw())
                + square_i32_saturating(diagonal.move_z.raw()),
        );
        assert!((length - Q12::SCALE).abs() <= 2);
    }

    #[test]
    fn decelerating_roll_preserves_range_with_a_fast_launch_and_slow_tail() {
        // Real Graybox cooked Q8 tuning: 46 authored units / 16 per tick.
        let mut cfg = config();
        cfg.roll_speed = 46 * 16;
        cfg.roll_active_frames = 60;
        cfg.roll_recovery_frames = 13;
        cfg.roll_decelerates = true;
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut positions = [0; 73];
        for (tick, position) in positions.iter_mut().enumerate() {
            let frame = motor.update(
                None,
                CharacterMotorInput {
                    move_z: Q12::ONE,
                    evade: tick == 0,
                    ..CharacterMotorInput::default()
                },
                cfg,
            );
            *position = frame.position.z;
        }
        assert_eq!(positions[59], 172);
        assert_eq!(positions[72], positions[59]);
        assert!((positions[21] - 110).abs() <= 1);
        assert!((positions[29] - 138).abs() <= 1);
        assert!((positions[37] - 155).abs() <= 1);
        assert!(motor.action().is_idle());
        for duration in [1, 14, 22, 60, 120, 255] {
            let total: i64 = (0..duration)
                .map(|frame| i64::from(decelerating_roll_step(736, frame, duration)))
                .sum();
            assert_eq!(total, 736 * i64::from(duration));
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
        assert_eq!(frame.position, RoomPoint::new(0, 0, 6));
    }

    #[test]
    fn free_movement_turns_at_yaw_step_and_travels_along_facing() {
        // Facing +Z, stick full +X: the first tick turns 16 q12 toward +X and
        // moves along the new facing (mostly +Z), not along the stick.
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
        assert_eq!(frame.yaw, Angle::from_q12(16));
        assert!(
            frame.position.z > frame.position.x,
            "travel follows the facing"
        );
        assert_eq!(frame.anim, CharacterMotorAnim::Walk);
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
            config_instant_turn(),
        );
        assert_eq!(frame.position, RoomPoint::new(8, 0, 0));
        assert_eq!(frame.yaw, Angle::QUARTER);
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
            config_instant_turn(),
        );
        assert_eq!(frame.position, RoomPoint::new(64, 0, 0));
        assert_eq!(frame.anim, CharacterMotorAnim::Run);
        assert!(frame.sprinting);
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
        assert_eq!(frame.position, RoomPoint::new(0, 0, -6));
    }

    #[test]
    fn disabled_run_capability_ignores_sprint_input() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let mut cfg = config_instant_turn();
        cfg.run_enabled = false;
        let frame = motor.update(
            None,
            CharacterMotorInput {
                move_x: Q12::ONE,
                move_z: Q12::ZERO,
                walk: 1,
                sprint: true,
                ..CharacterMotorInput::default()
            },
            cfg,
        );
        assert_eq!(frame.position, RoomPoint::new(32, 0, 0));
        assert_eq!(frame.anim, CharacterMotorAnim::Walk);
        assert!(!frame.sprinting);
        assert_eq!(frame.stamina_q12, DEFAULT_STAMINA_MAX_Q12);
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
        assert_eq!(frame.position, RoomPoint::new(0, 0, 6));
    }

    #[test]
    fn grounded_walk_keeps_feet_on_flat_floor() {
        // Regression: with gravity in place, ordinary walking on a flat floor
        // still keeps the feet glued at y=0 (no drift, no float).
        let mut motor = CharacterMotorState::new(RoomPoint::new(128, 0, 128), Angle::ZERO);
        let mut cfg = config();
        cfg.walk_speed = 64 << 8;
        for _ in 0..8 {
            let f = motor.update(
                None,
                CharacterMotorInput {
                    walk: 1,
                    ..CharacterMotorInput::default()
                },
                cfg,
            );
            assert_eq!(f.position.y, 0, "feet stay on the flat floor while walking");
        }
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
    fn unlimited_stamina_profile_never_drains_or_gates_sprint() {
        let mut motor = CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO);
        let cfg = config().without_stamina_limit();
        let input = CharacterMotorInput {
            walk: 1,
            sprint: true,
            ..CharacterMotorInput::default()
        };

        for _ in 0..2_000 {
            let frame = motor.update(None, input, cfg);
            assert!(frame.sprinting);
            assert_eq!(frame.stamina_q12, cfg.stamina_max_q12);
        }
        assert_eq!(cfg.sprint_min_q12, 0);
        assert_eq!(cfg.roll_cost_q12, 0);
        assert_eq!(cfg.backstep_cost_q12, 0);
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
}
