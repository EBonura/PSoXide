//! Souls-like game-entity runtime (phase 3 of
//! docs/game-runtime-plan.md): SoA state over cooked
//! [`LevelGameEntityRecord`]s with per-archetype tick dispatch across
//! the souls state grammar (Idle / Patrol / Aggro / Windup / Attack /
//! Recover / Staggered / Dead), adopted from hl-psx's prop AI shape
//! with two deliberate differences: thinking gates on the
//! portal-expanded ACTIVE ROOM set instead of PVS, and melee
//! windup/commit/punish is the first-class attack grammar.
//!
//! Movement is Character-bound and motor-honest (the phase-3 seam
//! note): speeds come from the cooked record's Character-derived
//! `walk_speed`/`run_speed` (patrol walks, chase runs), and every step
//! goes through a [`GameEntityMover`] the owning game backs with the
//! engine motor's [`commit_body_step`] -- the exact grid-collision
//! stand/slide/step rules the player uses. Blocked patrol and chase
//! movement steers on eight compass headings: keep a working heading,
//! re-aim at the goal on a per-entity countdown, and when blocked rank
//! the headings by how directly they close on the goal, with the reverse
//! last. This gives collision-aware local routing without a navmesh, heap
//! allocation, or an unbounded search. Attack CONTACT resolution is the
//! combat slice (see [`crate::combat`]). Games with retained actor poses
//! use [`Self::tick_delta_deferred`] to freeze each active
//! swing's exact attack clip/phase, resolve authored capsules from the
//! same pose the body and equipment consume, and then latch the hit
//! through [`Self::connect_deferred_attack`]. The legacy immediate
//! front arc remains for games without retained-pose combat.
//! With zero cooked records every entry point returns immediately, so
//! a record-free game pays a handful of cycles per tick (measured in
//! the phase-3 budget's idle A/B).
//!
//! Crate rules hold: no statics, no unsafe, capacities are `const N`
//! parameters, cooked data arrives as `&'static` psx-level records,
//! and [`GameEntities::EMPTY`] is all-zero so the owning game can keep
//! the state in link-time-zero storage (`.bss`).
//!
//! [`commit_body_step`]: psx_engine::character_motor::commit_body_step

mod animation;
mod flow;
mod spawn;
mod tactics;
pub use animation::{EntityAnim, GameEntityAnimation};
pub use tactics::{
    EnemyGoal, EnemyGoalResult, EnemyTacticalSnapshot, PredictedContact, STANCE_REASON_PARRY,
};

use crate::combat::{arc_hits_circle, MeleeArc};
use crate::combat_intent::CombatMove;
use crate::combatant::{
    stance_scaled, ActionProtection, Combatant, CombatantId, Defender, HitIn, HitKind, PoiseRule,
    Strike,
};
use crate::vitality::{CombatStanceConfig, VitalityChannelId};
use psx_engine::{
    Angle, CharacterMotorAnim, CharacterMotorConfig, CharacterMotorFrame, CharacterMotorInput,
    CharacterMotorState, RoomPoint,
};
use psx_level::combat_verbs::CombatKits;
use psx_level::{
    game_entity_flags, CharacterActionFrameRange, CharacterAnimationAction, LevelCharacterRecord,
    LevelGameEntityRecord, RoomIndex, CHARACTER_ACTION_SPEED_UNSCALED_Q8,
};
use psx_math::atan2_q12;

/// Souls behavior state for one entity. The all-zero pattern is
/// [`GameEntityState::Idle`], preserving the crate's zeroed-storage
/// discipline.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameEntityState {
    /// Holding position, scanning for a target.
    Idle = 0,
    /// Walking between the spawn anchor and the patrol anchor.
    Patrol = 1,
    /// Player noticed: closing distance.
    Aggro = 2,
    /// Attack telegraph; the record's `windup_ticks` long.
    Windup = 3,
    /// Attack active window (contact resolution is the combat slice).
    Attack = 4,
    /// Post-attack recovery; the record's `recovery_ticks` long (the
    /// punish window).
    Recover = 5,
    /// Poise broke (any break but a crit's): its Stun clip played through,
    /// the record's `stagger_ticks`.
    Staggered = 6,
    /// Health reached zero (or the record spawned disabled).
    Dead = 7,
    /// A crit broke poise: down and helpless for the record's
    /// `combat.knockdown_ticks`, then back up.
    KnockedDown = 8,
}

impl GameEntityState {
    /// Decode raw SoA storage. Unknown values read as [`Self::Dead`]
    /// so corrupted state fails inert, never hyperactive.
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Idle,
            1 => Self::Patrol,
            2 => Self::Aggro,
            3 => Self::Windup,
            4 => Self::Attack,
            5 => Self::Recover,
            6 => Self::Staggered,
            8 => Self::KnockedDown,
            _ => Self::Dead,
        }
    }
}

/// Short-lived combat movement choice layered over [`GameEntityState`].
/// The state machine owns committed actions; this intent only decides how an
/// engaged enemy behaves while it is still free to reconsider.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameEntityIntent {
    /// Face the target and wait.
    Hold = 0,
    /// Close distance toward the target.
    Approach = 1,
    /// Orbit counter-clockwise around the target.
    CircleLeft = 2,
    /// Orbit clockwise around the target.
    CircleRight = 3,
    /// Back away while keeping the target faced.
    Retreat = 4,
}

impl GameEntityIntent {
    /// Decode raw SoA storage. Unknown values fail to the inert hold intent.
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Approach,
            2 => Self::CircleLeft,
            3 => Self::CircleRight,
            4 => Self::Retreat,
            _ => Self::Hold,
        }
    }
}

/// Melee close-in margin added on top of the two body radii when
/// deriving attack reach: an entity commits to its windup when the
/// player is within `self.body(record).radius + player_radius + MARGIN` in XZ.
/// The radii are Character-bound (cooked from the same
/// `CharacterControllerSettings` the motors use); this constant is the
/// one runtime tuning knob (roughly half a demo-scale step) standing
/// in for per-weapon reach until the combat slice cooks real melee
/// arcs.
pub const GAME_ENTITY_ATTACK_REACH_MARGIN: i32 = 8;

/// Ticks the attack active window lasts.
pub const GAME_ENTITY_ATTACK_ACTIVE_TICKS: u16 = 6;

/// Packed selected-attack values. Bit 7 records which close-range variant is
/// next, keeping selection and alternation to one byte per entity.
const GAME_ENTITY_ATTACK_LIGHT: u8 = 0;
const GAME_ENTITY_ATTACK_HEAVY: u8 = 1;
const GAME_ENTITY_ATTACK_RANGED: u8 = 2;
const GAME_ENTITY_ATTACK_KIND_MASK: u8 = 3;
#[cfg(test)]
const GAME_ENTITY_ATTACK_MELEE_CHASE: u8 = 1 << 6;
#[cfg(test)]
const GAME_ENTITY_ATTACK_NEXT_HEAVY: u8 = 1 << 7;

/// One-hit-per-swing latch: set while the current Attack window already
/// connected.
const GAME_ENTITY_ATTACK_CONNECTED: u8 = 1;

/// The current stagger is a parry's riposte: the entity plays its Parried
/// clip over it. Cleared by every state change.
const GAME_ENTITY_PARRIED_STAGGER: u8 = 1 << 1;

/// Ticks the one-shot eye pulse lasts after a stance change.
const GAME_ENTITY_STANCE_PULSE_TICKS: u16 = 30;

/// The body of the entity `record` places, among the level's `characters`:
/// its linked Character, else [`LevelCharacterRecord::DEFAULT_BODY`].
// Read from dozens of call sites: one small copy on the PS1 rather than the
// bounds check and fallback inlined into each.
#[inline(never)]
pub fn body_of(
    characters: &'static [LevelCharacterRecord],
    record: &LevelGameEntityRecord,
) -> &'static LevelCharacterRecord {
    characters
        .get(record.character.to_usize())
        .unwrap_or(&LevelCharacterRecord::DEFAULT_BODY)
}

/// Front-arc half-width for entity attacks, PSX angle units
/// (60 degrees). The entity faced the player when it committed to
/// its windup; a player who rolls past the body during the swing
/// leaves this arc and the attack whiffs -- the souls punish loop.
pub const GAME_ENTITY_ATTACK_HALF_ANGLE: u16 = 683;

/// Legacy interruption duration for records without authored stun timing.
pub const GAME_ENTITY_STAGGER_TICKS: u16 = 32;

/// [`GameEntities::rival`] of an entity that engages the owner's target
/// (the tick input's, the player) rather than another slot.
pub const NO_RIVAL: u8 = u8::MAX;

/// How far a fighter's movement carries, in world units: nothing while it
/// stands, twice its height walking, four times running or evading. The one
/// rule for the player and every entity.
pub fn movement_noise(moved: bool, loud: bool, height: i32) -> i32 {
    if !moved {
        return 0;
    }
    height.max(0).saturating_mul(if loud { 4 } else { 2 })
}

/// De-aggro leash: the player escaping `aggro_radius` times this
/// factor drops the entity back to its idle/patrol loop.
pub const GAME_ENTITY_LEASH_FACTOR: i32 = 2;

/// One eighth turn in the engine's 4096-unit yaw representation.
const GAME_ENTITY_DIRECTION_STEP: u16 = 512;

/// Mask for the engine's 4096-unit yaw representation.
const GAME_ENTITY_YAW_MASK: u16 = 4095;

/// A motor input that asks for nothing: stand still, keep the facing.
const NO_MOTION: CharacterMotorInput = CharacterMotorInput {
    turn: 0,
    walk: 0,
    move_x: psx_engine::Q12::ZERO,
    move_z: psx_engine::Q12::ZERO,
    facing_yaw: None,
    sprint: false,
    evade: false,
};

/// The world a fighter moves through, supplied by the owning game each tick.
///
/// Every character moves through one mover: the player's
/// [`CharacterMotorState`]. An entity's body is a motor of its own, driven by
/// its controller's [`CharacterMotorInput`] under its own character's
/// [`CharacterMotorConfig`]; the owner advances it through the same collision
/// solve it runs for the player (grid floors, step rules, walls, prop and
/// actor blockers), so walking, running, rolling, stamina and i-frames follow
/// one set of rules whoever is moving.
pub trait FighterWorld {
    /// Advance the motor of entity `entity` (in `room`) by `delta` display
    /// frames under `input`. Every other body blocks it, never its own.
    #[allow(clippy::too_many_arguments)]
    fn move_body(
        &mut self,
        entity: usize,
        room: RoomIndex,
        motor: &mut CharacterMotorState,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta: u16,
    ) -> CharacterMotorFrame;

    /// Push the body of entity `entity` at `position` by `(dx, dz)`: a
    /// knockback, the same collision-checked push the owner gives the player
    /// on a hazard. Returns where the body ends up (`position` when blocked).
    #[allow(clippy::too_many_arguments)]
    fn push_body(
        &mut self,
        entity: usize,
        room: RoomIndex,
        position: [i32; 3],
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
    ) -> [i32; 3];

    /// The game's speed scale `(numerator, denominator)`, applied to every
    /// character's authored speeds, the player's included.
    fn speed_scale(&self) -> (i32, i32) {
        (1, 1)
    }

    /// Whether a point segment from an NPC's eye to the player's eye is clear
    /// of world geometry. Games without a visibility provider retain the
    /// previous behavior; a backend with one overrides this and fails closed.
    fn line_of_sight(&mut self, _room: RoomIndex, _from: [i32; 3], _to: [i32; 3]) -> bool {
        true
    }

    /// The intent of an owner that drives entity `entity` itself this think
    /// (a pad, a script): the possession seam. The entity's tactical
    /// controller then sits the think out and the rules run this intent
    /// exactly as they run the controller's, so whoever drives a fighter,
    /// the same rules move, swap, strike and shoot it. Only a tactical
    /// record in combat asks. `None` (the default) lets the tactical
    /// controller decide; a game that never possesses keeps the default and
    /// pays nothing for it.
    fn controller_intent(&mut self, _entity: usize) -> Option<crate::combat_intent::CombatIntent> {
        None
    }
}

/// Collision provider with nothing in it: no floor, no walls. A motor over
/// it moves freely and keeps its height.
pub struct OpenSpace;

impl psx_engine::CollisionTraceProvider for OpenSpace {
    fn trace_into(
        &mut self,
        _query: psx_engine::CollisionTraceQuery,
        _output: &mut psx_engine::CollisionTrace,
    ) -> bool {
        // The caller presets an unobstructed trace.
        true
    }
}

/// Open world: every body moves through [`OpenSpace`] with the real motor.
/// Host-test shape, and the honest fallback for games without collision.
pub struct OpenWorld;

impl FighterWorld for OpenWorld {
    fn move_body(
        &mut self,
        _entity: usize,
        _room: RoomIndex,
        motor: &mut CharacterMotorState,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta: u16,
    ) -> CharacterMotorFrame {
        motor
            .update_vblanks_with_trace_provider(&mut OpenSpace, input, config, delta)
            .expect("open space never fails")
    }

    fn push_body(
        &mut self,
        _entity: usize,
        _room: RoomIndex,
        position: [i32; 3],
        dx: i32,
        dz: i32,
        _radius: i32,
        _height: i32,
    ) -> [i32; 3] {
        [
            position[0].saturating_add(dx),
            position[1],
            position[2].saturating_add(dz),
        ]
    }
}

/// What an entity perceives of the fighter it is engaging this tick. The
/// owner fills it from whichever fighter that is; the entity's decisions
/// read only this record, never a particular character.
#[derive(Clone, Copy)]
pub struct CombatTarget<'a> {
    /// Target position, world/room-local engine units (the same space the
    /// cooked records use).
    pub position: [i32; 3],
    /// Target body radius, engine units (the target Character's capsule;
    /// the other half of Character-derived attack reach).
    pub radius: i32,
    /// Target body height, used to trace sight at torso height instead of
    /// along the floor.
    pub height: i32,
    /// Current movement noise radius, in world units. Zero means silent.
    pub noise_radius: i32,
    /// The target as a defender. Entity attacks ask it
    /// [`Defender::invulnerable`] and resolve no contact against an
    /// invulnerable target -- the swing stays live, so i-framing only the
    /// first half of a window still eats the tail (souls timing rules).
    pub defender: Defender<'a>,
    /// Visible target's active stance, for shared stance decisions. None
    /// retains distance-only policy for legacy callers and fixtures.
    pub stance: Option<VitalityChannelId>,
    /// The target's visible attack phase: 0 neutral, 1 tell, 2 swing,
    /// 3 recovery.
    pub attack_read: u8,
    /// Ticks until the target's swing first makes contact, 0 while it is
    /// live, None when it is not swinging: the owner's reading of the
    /// swing's first contact frame, the same reading the duel bot makes of
    /// an enemy's.
    pub swing_ticks: Option<u16>,
}

/// Per-tick inputs the owning game threads in: the target each entity
/// engages. Which entities think is the owner's spatial mask, see
/// [`GameEntities::set_spatial_active_mask`].
#[derive(Clone, Copy)]
pub struct GameEntityTickInput<'a> {
    /// The fighter the entities engage, as they perceive it.
    pub target: CombatTarget<'a>,
}

/// Per-tick outcome counters, for overlays and budget telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameEntityTickStats {
    /// Entities that ran their state machine this tick.
    pub thought: u16,
    /// Entities skipped by the spatial activation gate.
    pub gated: u16,
    /// Entities whose attack window is active this tick (the combat
    /// slice consumes these for contact resolution).
    pub attacking: u16,
    /// Transitions INTO Patrol this tick.
    pub patrol_enters: u16,
    /// Transitions INTO Aggro this tick.
    pub aggro_enters: u16,
    /// Transitions INTO Windup this tick.
    pub windup_enters: u16,
    /// Transitions INTO Attack this tick.
    pub attack_enters: u16,
    /// Melee attacks among this tick's Attack transitions.
    pub melee_attack_enters: u16,
    /// Ranged attacks among this tick's Attack transitions.
    pub ranged_attack_enters: u16,
    /// Times the combat director granted its shared attack slot.
    pub attack_grants: u16,
    /// Engaged enemies holding position this tick.
    pub holding: u16,
    /// Engaged enemies circling the player this tick.
    pub circling: u16,
    /// Engaged enemies retreating from the player this tick.
    pub retreating: u16,
    /// Entity attacks that CONNECTED with the player this tick.
    pub player_hits: u16,
    /// Total damage those connections apply to the player this tick
    /// (the owning game subtracts it from its player health).
    pub player_damage: u16,
}

/// A strike the player sends at an entity.
pub(crate) fn player_strike(
    kind: HitKind,
    channel: VitalityChannelId,
    damage: u16,
    poise: u16,
    crit: bool,
) -> Strike {
    Strike {
        attacker: CombatantId::PLAYER,
        kind,
        channel,
        damage,
        poise,
        crit,
    }
}

/// What one [`GameEntities::apply_hit`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameEntityHitOutcome {
    /// The hit landed on a live entity.
    pub connected: bool,
    /// The hit broke poise (entity is now [`GameEntityState::Staggered`],
    /// or [`GameEntityState::KnockedDown`] after a crit's break).
    pub staggered: bool,
    /// The break knocked the entity down (a crit's break).
    pub knocked_down: bool,
    /// The hit was lethal (entity is now [`GameEntityState::Dead`]).
    pub died: bool,
    /// The entity parried the hit: nothing landed on it (`connected` stays
    /// false). The owner staggers the attacker when the attacker is not an
    /// entity.
    pub parried: bool,
}

impl GameEntityHitOutcome {
    /// Out-of-range / already-dead target: nothing happened.
    pub const MISS: Self = Self {
        connected: false,
        staggered: false,
        knocked_down: false,
        died: false,
        parried: false,
    };
}

/// Animation selection for one entity this tick (the AI-state ->
/// animation-clip seam): which cooked model-local clip its bound
/// instance should play and where in the clip playback is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameEntityClip {
    /// Model-local clip index from the cooked record.
    pub clip: u16,
    /// 60 Hz ticks into the clip's playback.
    pub phase_ticks: u16,
    /// One-shot playback: clamp at the clip's final frame instead of
    /// looping.
    pub one_shot: bool,
    /// Q8 playback speed (`256 = 1.0x`). Combat actions carry the exact
    /// Animation Set option authored in the editor.
    pub speed_q8: u16,
    /// Inclusive source-frame window. Combat event tests and presentation
    /// therefore sample the same trimmed phase.
    pub frame_range: CharacterActionFrameRange,
}

impl Default for GameEntityClip {
    fn default() -> Self {
        Self {
            clip: 0,
            phase_ticks: 0,
            one_shot: false,
            speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            frame_range: CharacterActionFrameRange::FULL,
        }
    }
}

/// One attack contact opportunity frozen by [`GameEntities::tick_delta_deferred`].
///
/// The token captures the exact attack clip/phase and root transform before an
/// Attack-to-Recover transition can change the live state. It is valid only
/// until the next entity tick: both [`GameEntities::connect_deferred_attack`]
/// and [`GameEntities::deferred_attack_legacy_arc_hits`] reject stale tokens by
/// generation and swing sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferredGameEntityAttack {
    entity: u16,
    tick_generation: u16,
    swing_sequence: u16,
    clip: GameEntityClip,
    action: CharacterAnimationAction,
    ranged: bool,
    position: [i32; 3],
    yaw: u16,
    room: RoomIndex,
}

impl DeferredGameEntityAttack {
    const EMPTY: Self = Self {
        entity: 0,
        tick_generation: 0,
        swing_sequence: 0,
        clip: GameEntityClip {
            clip: 0,
            phase_ticks: 0,
            one_shot: false,
            speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            frame_range: CharacterActionFrameRange::FULL,
        },
        action: CharacterAnimationAction::LightAttack,
        ranged: false,
        position: [0; 3],
        yaw: 0,
        room: RoomIndex(0),
    };

    /// Cooked game-entity index whose swing produced this token.
    pub const fn entity(self) -> usize {
        self.entity as usize
    }

    /// Exact model clip/phase that body, equipment, and hit geometry use.
    pub const fn clip(self) -> GameEntityClip {
        self.clip
    }

    /// Authored action whose hitbox or projectile emitter owns this swing.
    pub const fn action(self) -> CharacterAnimationAction {
        self.action
    }

    /// Whether this committed swing is the projectile variant.
    pub const fn is_ranged(self) -> bool {
        self.ranged
    }

    /// Frozen room of the attacker for the contact tick.
    pub const fn room(self) -> RoomIndex {
        self.room
    }

    /// Frozen attacker root position for the contact tick, used by callers
    /// for world-occlusion segments alongside pose-backed capsule tests.
    pub const fn position(self) -> [i32; 3] {
        self.position
    }
}

/// Caller-owned fixed-capacity contact handoff for one entity tick.
///
/// `EMPTY` is all-zero and safe in `.bss`. A frame is overwritten by every
/// [`GameEntities::tick_delta_deferred`] call; overflow is reported explicitly
/// and never causes heap allocation.
pub struct DeferredGameEntityAttacks<const MAX_ATTACKS: usize> {
    attacks: [DeferredGameEntityAttack; MAX_ATTACKS],
    count: u16,
    overflow: u16,
}

impl<const MAX_ATTACKS: usize> DeferredGameEntityAttacks<MAX_ATTACKS> {
    /// All-zero fixed-capacity frame.
    pub const EMPTY: Self = Self {
        attacks: [DeferredGameEntityAttack::EMPTY; MAX_ATTACKS],
        count: 0,
        overflow: 0,
    };

    /// Forget the previous tick's tokens.
    pub fn clear(&mut self) {
        self.count = 0;
        self.overflow = 0;
    }

    /// Valid contact tokens in deterministic cooked-entity order.
    pub fn as_slice(&self) -> &[DeferredGameEntityAttack] {
        &self.attacks[..usize::from(self.count)]
    }

    /// Number of valid tokens in this frame.
    pub const fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether this frame contains no attack contact opportunities.
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Copy one token out without retaining a borrow of the frame owner.
    pub fn get(&self, index: usize) -> Option<DeferredGameEntityAttack> {
        self.as_slice().get(index).copied()
    }

    /// Active attacks that could not fit this frame.
    pub const fn overflow_count(&self) -> u16 {
        self.overflow
    }

    fn push(&mut self, attack: DeferredGameEntityAttack) {
        let count = usize::from(self.count);
        if count < MAX_ATTACKS && count < usize::from(u16::MAX) {
            self.attacks[count] = attack;
            self.count += 1;
        } else {
            self.overflow = self.overflow.saturating_add(1);
        }
    }
}

/// Aggregate outcome of one [`GameEntities::apply_melee_arc`] sweep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MeleeArcStats {
    /// Entities the sweep connected with.
    pub hits: u16,
    /// Connections that broke poise.
    pub staggers: u16,
    /// Connections that killed.
    pub deaths: u16,
    /// Connections the target parried.
    pub parries: u16,
}

/// SoA runtime state for cooked game entities. Entity `i` mirrors
/// `records[i]` of the slot-indexed record slice the owner passes: the boot
/// path ([`Self::spawn_from_records`]) fills slot `i` from global record `i`,
/// so links like `model_instance` stay index-stable; a pool fills slots one
/// at a time ([`Self::spawn_slot`]) with records of its own choosing and
/// names each by a stable spawn id. At most `MAX_ENTITIES` slots are live at
/// once: that is the size of the active set, not a limit on how many spawns a
/// world has.
/// Every fighter's attack family follows its stance (Horizon melee, Zenith
/// ranged).
pub struct GameEntities<const MAX_ENTITIES: usize> {
    tactics: [tactics::TacticalState; MAX_ENTITIES],
    /// Live entity count = `min(records.len(), MAX_ENTITIES)`.
    count: u16,
    /// Cooked records past `MAX_ENTITIES` that could not spawn.
    overflow: u16,
    /// Each entity's body: the same motor the player's character drives
    /// (position, facing, stamina, the roll and its i-frames, falling).
    motors: [CharacterMotorState; MAX_ENTITIES],
    /// What the entity being ticked asks its motor for this tick: written by
    /// its controller, spent by the one motor update at the end of its tick.
    motion: CharacterMotorInput,
    /// The compass heading the entity being ticked tries this tick, so the
    /// motor's answer (moved or not) steers the next tick's choice.
    probing: Option<u8>,
    /// Whether the entity being ticked has had its motor update this tick.
    driven: bool,
    /// The game's speed scale this tick (see [`FighterWorld::speed_scale`]).
    speed_scale: (i32, i32),
    /// Behavior state ([`GameEntityState`] as raw u8).
    state: [u8; MAX_ENTITIES],
    /// Ticks spent in the current state.
    state_ticks: [u16; MAX_ENTITIES],
    /// Health, poise, energy and stance of each entity: the same
    /// [`Combatant`] the player holds.
    combatants: [Combatant; MAX_ENTITIES],
    /// Each fighter's current action: its clip clock, lock and waiting
    /// attack and dodge requests ([`crate::fighter_action`]).
    actions: [crate::fighter_action::FighterAction; MAX_ENTITIES],
    /// The tactical controller's follow-up for the actor slot, kept until its
    /// next think (when the actor's own rules have run what it decided).
    actor_plan: Option<(tactics::Then, [i32; 3], Option<u8>)>,
    /// What held the actor at its latest think: 0 free, 1 an action, 2 an
    /// attack ([`Self::actor_intent`]).
    actor_busy: u8,
    /// How each entity's light, heavy and ranged attacks run: read off its
    /// Character's clips by [`Self::time_attacks`], else its record's ticks.
    attack_timings: [[crate::fighter_action::ActionTiming; 3]; MAX_ENTITIES],
    projectile_threats: [Option<crate::projectiles::ProjectileThreat>; MAX_ENTITIES],
    exchanges: [crate::ranged_tactics::RangedExchange; MAX_ENTITIES],
    recoil: [[i16; 3]; MAX_ENTITIES],
    /// The level's Characters: every entity's body is the one its record
    /// links ([`LevelGameEntityRecord::character`], read by [`Self::body`]).
    // An Option so the all-zero pattern (a game that keeps its entities in
    // zeroed storage) is a valid value: no table yet.
    characters: Option<&'static [LevelCharacterRecord]>,
    /// Each fighter's stance swap: how long a swap takes and the cooldown
    /// it starts, in 60 Hz ticks, from its Character (the actor's from
    /// hers, [`Self::set_swap_timing`]).
    swap_timing: [CombatStanceConfig; MAX_ENTITIES],
    /// World aim point frozen at the charge cutoff, independent of muzzle offset.
    ranged_aim_target: [[i32; 3]; MAX_ENTITIES],
    /// Patrol leg: 0 = toward the patrol anchor, 1 = toward spawn.
    patrol_leg: [u8; MAX_ENTITIES],
    /// Persistent eight-way local movement direction, in PSX yaw units.
    move_yaw: [u16; MAX_ENTITIES],
    /// 1 once `move_yaw` has been selected for the current behavior state.
    move_yaw_valid: [u8; MAX_ENTITIES],
    /// Directions rejected during the current bounded local search.
    move_tried: [u8; MAX_ENTITIES],
    /// Combat latch byte: [`GAME_ENTITY_ATTACK_CONNECTED`] while the current
    /// Attack window already connected.
    combat_flags: [u8; MAX_ENTITIES],
    /// Combat flash of each entity's model (presentation only).
    flash: [crate::combat_fx::CombatFlash; MAX_ENTITIES],
    /// Wrapping identity of each entity's current swing. Deferred tokens use
    /// it to reject a contact retained across a later attack.
    attack_sequence: [u16; MAX_ENTITIES],
    authored_connection_mask: [u16; MAX_ENTITIES],
    /// Packed selected swing plus next close-range alternation bit.
    attack_mode: [u8; MAX_ENTITIES],
    /// Free-movement combat choice ([`GameEntityIntent`] as raw u8).
    intent: [u8; MAX_ENTITIES],
    /// Remaining local post-attack cooldown in 60 Hz ticks.
    attack_cooldown: [u16; MAX_ENTITIES],
    /// Time spent waiting for the shared attack slot, used for fairness.
    attack_wait_ticks: [u16; MAX_ENTITIES],
    /// Shared attack-slot owner encoded as entity index + 1; zero means free.
    attack_owner_plus_one: u16,
    attack_owner_chase_ticks: u16,
    /// Remaining shared delay before another attack slot may be granted.
    director_delay_ticks: u16,
    /// Wrapping identity of the latest tick/tick-delta call. Deferred contact
    /// tokens are deliberately one-call capabilities.
    attack_tick_generation: u16,
    /// Stable spawn id of the record in each slot, [`psx_level::StableId::NONE`]
    /// (zero) for a slot spawned without one.
    spawn_ids: [u32; MAX_ENTITIES],
    /// The slot each entity fights, [`NO_RIVAL`] when it engages the
    /// owner's target (the player) from the tick input.
    rivals: [u8; MAX_ENTITIES],
    /// How each entity's body moved at its latest think: 0 still, 1
    /// walking, 2 running or evading ([`movement_noise`]).
    moving: [u8; MAX_ENTITIES],
    /// The side each entity's bolts and body are on.
    teams: [crate::projectiles::CombatTeam; MAX_ENTITIES],
    /// The animation intent of each entity's motor at its latest think:
    /// what its locomotion clip shows ([`Self::update_animation`]).
    motor_anims: [CharacterMotorAnim; MAX_ENTITIES],
    /// Each entity's shown clip and the switch it crossfades from
    /// ([`crate::fighter_anim::AnimTrack`]).
    bodies: [crate::fighter_anim::AnimTrack<EntityAnim>; MAX_ENTITIES],
    /// Clock of each shown clip: the tick its phase reads as zero.
    body_starts: [psx_engine::SimTick; MAX_ENTITIES],
    /// Each entity's locomotion (its gait, idle or turn), on the clock of
    /// its [`crate::fighter_action::FighterAction::start`]: the full body in
    /// tactical Aggro, the legs while it moves through an attack.
    gaits: [crate::fighter_anim::AnimTrack<EntityAnim>; MAX_ENTITIES],
    /// The tick of the latest [`Self::update_animation`].
    anim_clock: psx_engine::SimTick,
    /// What each entity shows, as the latest [`Self::update_animation`]
    /// resolved it.
    shown: [GameEntityAnimation; MAX_ENTITIES],
    /// Owner-supplied per-record activation (area residency).
    // psx-numeric-allow-next-line: fixed 64-record activation mask; bit ops only, two-word on R3000
    spatial_active_mask: u64,
}

impl<const MAX_ENTITIES: usize> GameEntities<MAX_ENTITIES>
{
    fn can_run(record: &LevelGameEntityRecord) -> bool {
        record.flags & game_entity_flags::CAN_RUN != 0
    }

    fn has_ranged_attack(record: &LevelGameEntityRecord) -> bool {
        record.flags & game_entity_flags::RANGED_ATTACK != 0
    }

    fn selected_attack_kind(&self, index: usize) -> u8 {
        self.attack_mode[index] & GAME_ENTITY_ATTACK_KIND_MASK
    }

    fn selected_attack_is_ranged(&self, index: usize) -> bool {
        self.selected_attack_kind(index) == GAME_ENTITY_ATTACK_RANGED
    }

    fn selected_attack_clip(&self, record: &LevelGameEntityRecord, index: usize) -> u16 {
        match self.selected_attack_kind(index) {
            GAME_ENTITY_ATTACK_HEAVY => record.heavy_attack_clip,
            GAME_ENTITY_ATTACK_RANGED => record.ranged_attack_clip,
            _ => record.attack_clip,
        }
    }

    /// How entity `index`'s selected attack runs ([`Self::time_attacks`]).
    fn selected_timing(&self, index: usize) -> crate::fighter_action::ActionTiming {
        self.attack_timings[index][usize::from(self.selected_attack_kind(index)).min(2)]
    }

    /// The timing an attack of `kind` has from its record alone: the
    /// authored windup and recovery around the cooked active ticks.
    pub(crate) fn record_timings(
        record: &LevelGameEntityRecord,
    ) -> [crate::fighter_action::ActionTiming; 3] {
        let timing = |active: u16| crate::fighter_action::ActionTiming {
            windup: u16::from(record.windup_ticks),
            active: active.max(GAME_ENTITY_ATTACK_ACTIVE_TICKS),
            recovery: u16::from(record.recovery_ticks),
            armour: None,
            invulnerable: None,
        };
        [
            timing(record.attack_active_ticks),
            timing(record.heavy_attack_active_ticks),
            timing(record.ranged_attack_active_ticks),
        ]
    }

    /// Time every entity's attacks from its Character's clips: `timing` reads
    /// one attack (the record, the action, and the clip, speed and frame
    /// range the entity plays for it) and answers None when the clip authors
    /// no contact, which keeps the record's ticks. The same clip rules time
    /// the player's actions. Call after spawning; a spawn resets to the
    /// record's ticks.
    pub fn time_attacks(
        &mut self,
        records: &[LevelGameEntityRecord],
        mut timing: impl FnMut(
            &LevelGameEntityRecord,
            CharacterAnimationAction,
            u16,
            u16,
            CharacterActionFrameRange,
        ) -> Option<crate::fighter_action::ActionTiming>,
    ) {
        let count = self.count().min(records.len());
        for (index, record) in records.iter().enumerate().take(count) {
            let attacks = [
                (
                    CharacterAnimationAction::LightAttack,
                    record.attack_clip,
                    record.attack_speed_q8,
                    record.attack_frame_range,
                ),
                (
                    CharacterAnimationAction::HeavyAttack,
                    record.heavy_attack_clip,
                    record.heavy_attack_speed_q8,
                    record.heavy_attack_frame_range,
                ),
                (
                    CharacterAnimationAction::from_index(usize::from(record.ranged_attack_action))
                        .unwrap_or(CharacterAnimationAction::RangedAttack),
                    record.ranged_attack_clip,
                    record.ranged_attack_speed_q8,
                    record.ranged_attack_frame_range,
                ),
            ];
            for (kind, (action, clip, speed_q8, range)) in attacks.into_iter().enumerate() {
                if let Some(read) = timing(record, action, clip, speed_q8, range) {
                    self.attack_timings[index][kind] = read;
                }
            }
        }
    }

    fn selected_attack_playback(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
    ) -> (u16, CharacterActionFrameRange) {
        match self.selected_attack_kind(index) {
            GAME_ENTITY_ATTACK_HEAVY => (
                record.heavy_attack_speed_q8,
                record.heavy_attack_frame_range,
            ),
            GAME_ENTITY_ATTACK_RANGED => (
                record.ranged_attack_speed_q8,
                record.ranged_attack_frame_range,
            ),
            _ => (record.attack_speed_q8, record.attack_frame_range),
        }
    }

    /// The attack entity `index` is committed to while it winds up or
    /// swings, as a watcher reads it: the action, and the swing ticks over
    /// which its record times contact (from the end of the windup through
    /// its active ticks). The swing's clock is
    /// [`Self::clip_for_state`]'s `phase_ticks`. None when it is not
    /// attacking.
    pub fn committed_swing(
        &self,
        records: &[LevelGameEntityRecord],
        index: usize,
    ) -> Option<(CharacterAnimationAction, u16, u16)> {
        let record = records.get(index).filter(|_| index < self.count())?;
        if !matches!(
            self.state(index),
            GameEntityState::Windup | GameEntityState::Attack
        ) {
            return None;
        }
        let timing = self.selected_timing(index);
        let first = timing.windup;
        let last = first.saturating_add(timing.active).saturating_sub(1);
        Some((self.selected_attack_action(record, index), first, last))
    }

    fn selected_attack_action(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
    ) -> CharacterAnimationAction {
        match self.selected_attack_kind(index) {
            GAME_ENTITY_ATTACK_HEAVY => CharacterAnimationAction::HeavyAttack,
            GAME_ENTITY_ATTACK_RANGED => {
                CharacterAnimationAction::from_index(usize::from(record.ranged_attack_action))
                    .unwrap_or(CharacterAnimationAction::RangedAttack)
            }
            _ => CharacterAnimationAction::LightAttack,
        }
    }

    /// Choose the attack family from the committed stance when enabled.
    /// Other hybrids use distance with hysteresis. Enter melee chase at
    /// the outer edge of the authored preferred band (never inside the projectile minimum),
    /// but do not return to ranged until the player has also crossed the
    /// authored spacing tolerance.
    fn update_melee_chase(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
    ) -> bool {
        let moves = Self::stance_moves(record);
        if !moves.can_shoot() {
            return true;
        }
        if !moves.can_melee() {
            return false;
        }
        let was_melee = self.stance(index) == VitalityChannelId::One;
        // Spacing can stop at preferred + tolerance. Enter at that outer
        // edge too, or a hybrid can sit just outside melee forever.
        let melee_entry = i32::from(record.preferred_distance.max(record.attack_min_range))
            .saturating_add(i32::from(record.spacing_tolerance));
        let limit = crate::combat_policy::melee_limit(
            melee_entry,
            i32::from(record.spacing_tolerance),
            was_melee,
        );
        self.target_within(index, input, limit)
    }

    /// Commit one action for the entire Windup/Attack/Recover grammar. Close
    /// attacks alternate Light, Heavy, Light, Heavy without consuming the
    /// alternation on ranged shots. Tests only: the tactical controller picks
    /// its attack itself.
    #[cfg(test)]
    fn select_attack(&mut self, index: usize, ranged: bool) {
        let persistent = self.attack_mode[index]
            & (GAME_ENTITY_ATTACK_MELEE_CHASE | GAME_ENTITY_ATTACK_NEXT_HEAVY);
        self.attack_mode[index] = if ranged {
            persistent | GAME_ENTITY_ATTACK_RANGED
        } else if persistent & GAME_ENTITY_ATTACK_NEXT_HEAVY != 0 {
            GAME_ENTITY_ATTACK_MELEE_CHASE | GAME_ENTITY_ATTACK_HEAVY
        } else {
            GAME_ENTITY_ATTACK_MELEE_CHASE
                | GAME_ENTITY_ATTACK_NEXT_HEAVY
                | GAME_ENTITY_ATTACK_LIGHT
        };
    }

    fn approach_clip(record: &LevelGameEntityRecord) -> u16 {
        if Self::can_run(record) {
            record.run_clip
        } else {
            record.walk_clip
        }
    }

    /// All-zero state; `const` so the owning game can keep it in
    /// link-time-zero (`.bss`) scene storage. Not meaningful until
    /// [`Self::spawn_from_records`] runs.
    pub const EMPTY: Self = Self {
        tactics: [tactics::TacticalState::EMPTY; MAX_ENTITIES],
        count: 0,
        overflow: 0,
        motors: [CharacterMotorState::new(RoomPoint::ZERO, Angle::ZERO); MAX_ENTITIES],
        motion: NO_MOTION,
        probing: None,
        driven: false,
        speed_scale: (1, 1),
        state: [0; MAX_ENTITIES],
        state_ticks: [0; MAX_ENTITIES],
        combatants: [Combatant::EMPTY; MAX_ENTITIES],
        actions: [crate::fighter_action::FighterAction::IDLE; MAX_ENTITIES],
        actor_plan: None,
        actor_busy: 0,
        attack_timings: [[crate::fighter_action::ActionTiming {
            windup: 0,
            active: 0,
            recovery: 0,
            armour: None,
            invulnerable: None,
        }; 3]; MAX_ENTITIES],
        projectile_threats: [None; MAX_ENTITIES],
        exchanges: [crate::ranged_tactics::RangedExchange::EMPTY; MAX_ENTITIES],
        recoil: [[0; 3]; MAX_ENTITIES],
        #[cfg(not(test))]
        characters: None,
        #[cfg(test)]
        characters: Some(&tests::TEST_BODIES),
        swap_timing: [CombatStanceConfig::DEFAULT; MAX_ENTITIES],
        ranged_aim_target: [[0; 3]; MAX_ENTITIES],
        patrol_leg: [0; MAX_ENTITIES],
        move_yaw: [0; MAX_ENTITIES],
        move_yaw_valid: [0; MAX_ENTITIES],
        move_tried: [0; MAX_ENTITIES],
        combat_flags: [0; MAX_ENTITIES],
        flash: [crate::combat_fx::CombatFlash::NONE; MAX_ENTITIES],
        attack_sequence: [0; MAX_ENTITIES],
        authored_connection_mask: [0; MAX_ENTITIES],
        attack_mode: [0; MAX_ENTITIES],
        intent: [0; MAX_ENTITIES],
        attack_cooldown: [0; MAX_ENTITIES],
        attack_wait_ticks: [0; MAX_ENTITIES],
        attack_owner_plus_one: 0,
        attack_owner_chase_ticks: 0,
        director_delay_ticks: 0,
        attack_tick_generation: 0,
        spawn_ids: [0; MAX_ENTITIES],
        rivals: [NO_RIVAL; MAX_ENTITIES],
        moving: [0; MAX_ENTITIES],
        teams: [crate::projectiles::CombatTeam::Enemy; MAX_ENTITIES],
        motor_anims: [CharacterMotorAnim::Idle; MAX_ENTITIES],
        bodies: [crate::fighter_anim::AnimTrack::new(EntityAnim::NONE); MAX_ENTITIES],
        body_starts: [psx_engine::SimTick::ZERO; MAX_ENTITIES],
        gaits: [crate::fighter_anim::AnimTrack::new(EntityAnim::NONE); MAX_ENTITIES],
        anim_clock: psx_engine::SimTick::ZERO,
        shown: [GameEntityAnimation::NONE; MAX_ENTITIES],
        spatial_active_mask: 0,
    };

    /// Reset and spawn entity state 1:1 from the cooked records
    /// (souls checkpoint-respawn calls this again on death loops).
    /// Records past `MAX_ENTITIES` count into
    /// [`Self::overflow_count`]; records without
    /// `game_entity_flags::ENABLED` spawn [`GameEntityState::Dead`]
    /// so indices stay stable.
    /// The actor slot ([`Self::ACTOR_SLOT`]) keeps its body and fighter when
    /// the records leave it free.
    // Shared by initial load, New Game and respawn; keep one copy on PS1.
    #[inline(never)]
    pub fn spawn_from_records(&mut self, records: &'static [LevelGameEntityRecord]) {
        let actor = (
            self.motors[Self::ACTOR_SLOT],
            self.combatants[Self::ACTOR_SLOT],
            self.actions[Self::ACTOR_SLOT],
            self.swap_timing[Self::ACTOR_SLOT],
        );
        // The level's Characters outlive a respawn of its entities.
        let characters = self.characters;
        *self = Self::EMPTY;
        self.characters = characters;
        let count = records.len().min(MAX_ENTITIES);
        if count <= Self::ACTOR_SLOT {
            self.motors[Self::ACTOR_SLOT] = actor.0;
            self.combatants[Self::ACTOR_SLOT] = actor.1;
            self.actions[Self::ACTOR_SLOT] = actor.2;
            self.swap_timing[Self::ACTOR_SLOT] = actor.3;
        }
        self.count = count as u16;
        self.overflow = (records.len() - count).min(u16::MAX as usize) as u16;
        for (index, record) in records.iter().enumerate().take(count) {
            self.init_slot(index, record, 0x9e3779b9u32.wrapping_add(index as u32 * 97));
        }
    }

    /// Live entity count.
    pub fn count(&self) -> usize {
        usize::from(self.count)
    }

    /// All enabled enemies were defeated. Empty/disabled-only encounters and
    /// truncated spawns cannot report success.
    pub fn encounter_cleared(&self, records: &[LevelGameEntityRecord]) -> bool {
        if self.overflow != 0 || self.count() < records.len() {
            return false;
        }
        let mut any = false;
        for (index, record) in records.iter().enumerate() {
            if record.flags & game_entity_flags::ENABLED == 0 {
                continue;
            }
            any = true;
            if self.state(index) != GameEntityState::Dead {
                return false;
            }
        }
        any
    }

    /// The level's Characters (its generated `CHARACTERS` table), where
    /// every entity's body is read: combat numbers, capsule, motion, stance
    /// swap timing, combat windows and volumes. Set before spawning; an
    /// entity whose link is past the table has
    /// [`LevelCharacterRecord::DEFAULT_BODY`].
    pub fn set_characters(&mut self, characters: &'static [LevelCharacterRecord]) {
        self.characters = Some(characters);
    }

    /// Set slot `index`'s stance swap timing: the actor's from her
    /// Character (an entity's is set from its own when it spawns).
    pub fn set_swap_timing(&mut self, index: usize, timing: CombatStanceConfig) {
        if let Some(slot) = self.swap_timing.get_mut(index) {
            *slot = timing;
        }
    }

    /// Slot `index`'s stance swap timing.
    pub fn swap_timing(&self, index: usize) -> CombatStanceConfig {
        self.swap_timing
            .get(index)
            .copied()
            .unwrap_or(CombatStanceConfig::DEFAULT)
    }

    /// Test hook: every slot's swap cooldown, as the one global the
    /// tests were written against.
    #[cfg(test)]
    fn set_swap_cooldown_for_tests(&mut self, ticks: u16) {
        for timing in &mut self.swap_timing {
            timing.swap_cooldown_ticks = ticks;
        }
    }

    /// The body of the entity `record` places: its linked Character, the
    /// same record the player's character is ([`body_of`]).
    #[inline(always)]
    pub fn body(&self, record: &LevelGameEntityRecord) -> &'static LevelCharacterRecord {
        body_of(self.characters(), record)
    }

    /// The level's Characters ([`Self::set_characters`]).
    pub fn characters(&self) -> &'static [LevelCharacterRecord] {
        self.characters.unwrap_or(&[])
    }

    /// Remaining voluntary guard-swap cooldown for a living enemy.
    pub fn stance_swap_cooldown(&self, index: usize) -> u16 {
        self.combatants
            .get(index)
            .map_or(0, |c| c.stance.swap_cooldown())
    }

    /// Cooked records that did not fit `MAX_ENTITIES` at spawn.
    pub fn overflow_count(&self) -> u16 {
        self.overflow
    }

    /// Select which records the owner considers spatially active (bit `i`
    /// is record `i`). An idle or patrolling entity only thinks while its bit
    /// is set or the player is inside its notice range.
    // psx-numeric-allow-next-line: mirrors the fixed 64-record activation mask above; bit ops only
    pub fn set_spatial_active_mask(&mut self, mask: u64) {
        self.spatial_active_mask = mask;
    }

    /// Ticks entity `index` has spent in its current behavior state.
    pub fn state_age(&self, index: usize) -> u16 {
        self.state_ticks.get(index).copied().unwrap_or(0)
    }

    /// Start entity `index`'s flash of `kind` in the stance colour `zenith`
    /// (a parry's own, or a hit's attacker's). A parry flash is not replaced
    /// by a hit. Presentation only: nothing in the simulation reads it.
    pub fn start_flash(
        &mut self,
        index: usize,
        data: &crate::combat_fx::CombatFeedback,
        kind: crate::combat_fx::FlashKind,
        zenith: bool,
    ) {
        if let Some(flash) = self.flash.get_mut(index) {
            flash.begin(data, kind, zenith);
        }
    }

    /// Entity `index`'s combat flash ([`CombatFlash::NONE`] when it has none).
    ///
    /// [`CombatFlash::NONE`]: crate::combat_fx::CombatFlash::NONE
    pub fn flash(&self, index: usize) -> crate::combat_fx::CombatFlash {
        self.flash
            .get(index)
            .copied()
            .unwrap_or(crate::combat_fx::CombatFlash::NONE)
    }

    /// Whether any entity's model is flashing (a cheap gate for the draw).
    pub fn any_flash(&self) -> bool {
        self.flash.iter().any(|flash| flash.is_active())
    }

    /// Advance every entity's combat flash one 60 Hz simulation tick.
    pub fn tick_flashes(&mut self) {
        for flash in &mut self.flash {
            flash.tick();
        }
    }

    /// Whether entity `index`'s current stagger is a parry's riposte, over
    /// which it plays its Parried clip.
    pub fn parried_stagger(&self, index: usize) -> bool {
        self.state(index) == GameEntityState::Staggered
            && self.combat_flags[index] & GAME_ENTITY_PARRIED_STAGGER != 0
    }

    /// Behavior state of entity `index`; out-of-range indices read as `Dead`.
    pub fn state(&self, index: usize) -> GameEntityState {
        if index >= self.count() {
            return GameEntityState::Dead;
        }
        GameEntityState::from_raw(self.state[index])
    }

    /// Current reconsiderable combat movement intent for entity `index`.
    pub fn intent(&self, index: usize) -> GameEntityIntent {
        if index >= self.count() {
            return GameEntityIntent::Hold;
        }
        GameEntityIntent::from_raw(self.intent[index])
    }

    /// Entity currently allowed to approach and start an attack, if any.
    pub fn attack_owner(&self) -> Option<usize> {
        self.attack_owner_plus_one
            .checked_sub(1)
            .map(usize::from)
            .filter(|index| *index < self.count())
    }

    /// Whether slot `index` holds a fighter: a live entity, or the actor.
    fn holds_fighter(&self, index: usize) -> bool {
        index < self.count() || index == Self::ACTOR_SLOT
    }

    /// Position of entity `index` (or of the actor's body in
    /// [`Self::ACTOR_SLOT`]), room-local engine units.
    pub fn position(&self, index: usize) -> [i32; 3] {
        if !self.holds_fighter(index) {
            return [0; 3];
        }
        let p = self.motors[index].position();
        [p.x, p.y, p.z]
    }

    /// Current position of the living entity bound to a cooked model
    /// instance. Returns `None` when the instance is not entity-owned,
    /// was truncated at spawn, or its entity is dead.
    pub fn live_position_for_model_instance(
        &self,
        records: &[LevelGameEntityRecord],
        model_instance: u16,
    ) -> Option<[i32; 3]> {
        if model_instance == psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE {
            return None;
        }
        let index = records
            .iter()
            .take(self.count())
            .position(|record| record.model_instance == model_instance)?;
        (self.state(index) != GameEntityState::Dead).then(|| self.position(index))
    }

    /// Facing yaw of entity `index` (or of the actor's body), PSX angle
    /// units.
    pub fn yaw(&self, index: usize) -> i16 {
        if !self.holds_fighter(index) {
            0
        } else {
            self.yaw_raw(index)
        }
    }

    /// Remaining health of entity `index`.
    pub fn health(&self, index: usize) -> u16 {
        if !self.holds_fighter(index) {
            0
        } else {
            self.combatants[index].health()
        }
    }

    /// Vitality channel currently guarded by entity `index`.
    /// Out-of-range entities fail to Horizon, the side a fresh combatant holds.
    pub fn stance(&self, index: usize) -> VitalityChannelId {
        if self.holds_fighter(index) {
            self.combatants[index].stance.active()
        } else {
            VitalityChannelId::One
        }
    }

    /// Q12 presentation progress for the entity's current guard mutation.
    /// Settled and invalid entities return one, matching player HUD semantics.
    pub fn stance_swap_progress_q12(&self, index: usize) -> u16 {
        if !self.holds_fighter(index) {
            return 4096;
        }
        let elapsed = self.stance_swap_elapsed(index);
        let duration = u32::from(self.swap_timing[index].swap_duration_ticks).max(1);
        ((u32::from(elapsed) * 4096) / duration).min(4096) as u16
    }

    /// Whether the entity's rising stance-colour tell is still active.
    pub fn stance_swap_in_progress(&self, index: usize) -> bool {
        self.holds_fighter(index)
            && self.stance_swap_elapsed(index) < self.swap_timing[index].swap_duration_ticks
    }

    /// Half-second eye pulse, once per actual stance change. No pulse on spawn.
    pub fn stance_eye_pulse_q12(&self, index: usize) -> Option<u16> {
        if index >= self.count() || self.state(index) == GameEntityState::Dead {
            return None;
        }
        let age = self.stance_swap_elapsed(index);
        (age < GAME_ENTITY_STANCE_PULSE_TICKS).then(|| age * 4096 / 30)
    }

    /// Health damage `damage` on `attack` would deal entity `index` under
    /// the one stance rule (its body's opposite-stance multiplier). What
    /// the pools then absorb can be less. Zero stays zero.
    pub fn scaled_stance_damage(
        &self,
        records: &[LevelGameEntityRecord],
        index: usize,
        attack: VitalityChannelId,
        damage: u16,
    ) -> u16 {
        match (records.get(index), self.combatants.get(index)) {
            (Some(record), Some(combatant)) => {
                stance_scaled(
                    &self.body(record).combat,
                    combatant.opposes(attack),
                    damage,
                    0,
                )
                .0
            }
            _ => damage,
        }
    }

    /// Entity `index` as the target of a hit: its swap window for its
    /// body's `roll_invulnerable_frames` (the field the player character's
    /// swap window reads), its attack's authored `Invulnerable` window and
    /// its roll's i-frames. `None` for an out of range slot.
    // Cold AI-side helper: one small copy on the PS1 instead of one per caller.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub fn defender<'a>(
        &'a self,
        records: &[LevelGameEntityRecord],
        index: usize,
    ) -> Option<Defender<'a>> {
        if index >= self.count() {
            return None;
        }
        let record = records.get(index)?;
        Some(self.defender_with(record, index, self.combatants.get(index)?))
    }

    /// Entity `index` as a defender, over `combatant` (its own fighter or a
    /// copy of it).
    fn defender_with<'a>(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
        combatant: &'a Combatant,
    ) -> Defender<'a> {
        Defender {
            combatant,
            swap_invulnerable_ticks: self.body(record).roll_invulnerable_frames,
            parry_ticks: self.body(record).combat.parry_ticks,
            // The attack's authored `Invulnerable` window from its
            // Character, on the attack's clock as the armour window is; the
            // roll's i-frames from the motor exactly as the player's are.
            action: ActionProtection {
                authored: self
                    .attack_since(index)
                    .and_then(|since| self.selected_timing(index).invulnerable_at(since)),
                dodge: self.motors[index].is_action_invulnerable(self.motor_config(record)),
            },
        }
    }

    /// An idle or patrolling entity a hit reached turns on whoever dealt it,
    /// as if it had noticed them there: an entity at its slot, the player at
    /// the actor slot. The world's hits (hazards) wake it without a place.
    // Rare: a hit on an unaware fighter.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    /// Entity `index` turns on `attacker` now, as when a hit reaches it: it
    /// remembers where the attacker stands and enters Aggro (its reaction
    /// pause included). A duel starts its fighters engaged this way.
    pub fn engage(&mut self, index: usize, attacker: CombatantId) {
        if index < self.count() && self.state(index) != GameEntityState::Dead {
            self.notice_attacker(index, attacker);
        }
    }

    /// The clocks a fighter's tactical controller runs on, advanced once per
    /// think: its return-home cooldown and its ranged exchange's timers
    /// (cover, peek, decision and dodge delays). Every controller's think
    /// advances them, the actor's included.
    fn advance_controller_clocks(&mut self, index: usize, delta: u16) {
        self.tactics[index].home_cooldown = self.tactics[index].home_cooldown.saturating_sub(delta);
        self.exchanges[index].tick(delta);
    }

    fn notice_attacker(&mut self, index: usize, attacker: CombatantId) {
        let slot = if attacker == CombatantId::PLAYER {
            (self.count() <= Self::ACTOR_SLOT).then_some(Self::ACTOR_SLOT)
        } else {
            Some(usize::from(attacker.0)).filter(|&slot| slot < self.count())
        };
        if let Some(slot) = slot {
            let position = self.motors[slot].position();
            self.remember_target(index, [position.x, position.y, position.z]);
        }
        self.enter_state(
            index,
            GameEntityState::Aggro,
            &mut GameEntityTickStats::default(),
        );
    }

    /// The rival of entity `index` while it is a live slot of this tick.
    fn live_rival(&self, index: usize, count: usize) -> Option<usize> {
        let rival = usize::from(self.rivals[index]);
        (self.rivals[index] != NO_RIVAL && rival < count).then_some(rival)
    }

    /// Entity `rival` as the target another entity engages: what the owner
    /// supplies for the player, read off the rival's own slot. Its attack
    /// phase and swing timing come from its state clock (the record's
    /// windup), where the owner reads the player's off her pose.
    // Only fights between entities build one; keep it out of the hot tick.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn rival_target<'f>(
        &self,
        record: &LevelGameEntityRecord,
        rival: usize,
        fighter: &'f Combatant,
    ) -> CombatTarget<'f> {
        let state = self.state(rival);
        CombatTarget {
            position: self.position(rival),
            radius: i32::from(self.body(record).radius),
            height: i32::from(self.body(record).height),
            noise_radius: movement_noise(
                self.moving[rival] != 0,
                self.moving[rival] > 1,
                i32::from(self.body(record).height),
            ),
            defender: self.defender_with(record, rival, fighter),
            stance: Some(fighter.stance.active()),
            attack_read: match state {
                GameEntityState::Windup => 1,
                GameEntityState::Attack => 2,
                GameEntityState::Recover => 3,
                _ => 0,
            },
            swing_ticks: match state {
                GameEntityState::Windup => Some(
                    self.selected_timing(rival)
                        .windup
                        .saturating_sub(self.state_ticks[rival]),
                ),
                GameEntityState::Attack => Some(0),
                _ => None,
            },
        }
    }

    /// Set entity `index` to fight the entity in slot `rival`, on side
    /// `team` (its bolts hit the other sides, and theirs hit it), or with
    /// `None` to engage the owner's target again. A spawn clears every
    /// rival and puts every entity on [`crate::projectiles::CombatTeam::Enemy`].
    /// False when a slot is out of range.
    pub fn set_rival(
        &mut self,
        index: usize,
        rival: Option<usize>,
        team: crate::projectiles::CombatTeam,
    ) -> bool {
        let raw = match rival {
            Some(slot) if slot < MAX_ENTITIES && slot < usize::from(NO_RIVAL) && slot != index => {
                slot as u8
            }
            Some(_) => return false,
            None => NO_RIVAL,
        };
        if index >= MAX_ENTITIES {
            return false;
        }
        self.rivals[index] = raw;
        self.teams[index] = team;
        true
    }

    /// The slot entity `index` fights, None when it engages the owner's
    /// target ([`Self::set_rival`]).
    pub fn rival(&self, index: usize) -> Option<usize> {
        let raw = *self.rivals.get(index)?;
        (raw != NO_RIVAL).then_some(usize::from(raw))
    }

    /// The side entity `index` is on ([`Self::set_rival`]).
    pub fn team(&self, index: usize) -> crate::projectiles::CombatTeam {
        self.teams
            .get(index)
            .copied()
            .unwrap_or(crate::projectiles::CombatTeam::Enemy)
    }

    /// The last slot: the fighter a controller outside the table drives (the
    /// player's character). An owner that sizes the table one past its
    /// records gets a slot the entity tick never runs and a spawn never
    /// clears; its body and fighter are read and written through
    /// [`Self::actor_motor`] and [`Self::actor`]. When the records fill the
    /// table the slot is an ordinary entity.
    pub const ACTOR_SLOT: usize = MAX_ENTITIES - 1;

    /// The actor slot's body ([`Self::ACTOR_SLOT`]).
    #[inline(always)]
    pub fn actor_motor(&self) -> &CharacterMotorState {
        &self.motors[Self::ACTOR_SLOT]
    }

    /// The actor slot's body, to drive ([`Self::ACTOR_SLOT`]).
    #[inline(always)]
    pub fn actor_motor_mut(&mut self) -> &mut CharacterMotorState {
        &mut self.motors[Self::ACTOR_SLOT]
    }

    /// The actor slot's health, poise, energy and stance
    /// ([`Self::ACTOR_SLOT`]).
    #[inline(always)]
    pub fn actor(&self) -> &Combatant {
        &self.combatants[Self::ACTOR_SLOT]
    }

    /// The actor slot's fighter, to change ([`Self::ACTOR_SLOT`]).
    #[inline(always)]
    pub fn actor_mut(&mut self) -> &mut Combatant {
        &mut self.combatants[Self::ACTOR_SLOT]
    }

    /// The actor's current action ([`crate::fighter_action::FighterAction`]).
    pub fn actor_action(&self) -> &crate::fighter_action::FighterAction {
        &self.actions[Self::ACTOR_SLOT]
    }

    /// Mutable [`Self::actor_action`].
    pub fn actor_action_mut(&mut self) -> &mut crate::fighter_action::FighterAction {
        &mut self.actions[Self::ACTOR_SLOT]
    }

    /// The body of entity `index`: the motor the player's character drives
    /// too. None out of range.
    pub fn motor(&self, index: usize) -> Option<&CharacterMotorState> {
        self.motors.get(index).filter(|_| index < self.count())
    }

    /// Whether entity `index` is inside its i-frames this tick, see
    /// [`Self::defender`].
    pub fn invulnerable(&self, records: &[LevelGameEntityRecord], index: usize) -> bool {
        self.defender(records, index)
            .is_some_and(|defender| defender.invulnerable())
    }

    /// Whether a fighter's hit on entity `index` passes through it this tick
    /// ([`Defender::passes_through`]): inside its i-frames but past its
    /// parry window. Hit resolvers ask before a hit may connect, so a hit
    /// that passes through neither lands nor latches the swing; a hit in the
    /// parry window lands and is parried.
    pub fn passes_through(&self, records: &[LevelGameEntityRecord], index: usize) -> bool {
        self.defender(records, index)
            .is_some_and(|defender| defender.passes_through())
    }

    /// The moves entity `index`'s record gives it, which decide whether it
    /// may swap stance: the kits the cook derived from its bindings
    /// ([`CombatKits`], the same derivation as the player's).
    fn stance_moves(record: &LevelGameEntityRecord) -> CombatKits {
        psx_level::game_entity_combat_kits(record.flags)
    }

    /// Damage and poise of an attack entity `index` deals, through its crit
    /// ([`Combatant::crit_scaled`]).
    pub fn crit_scaled(
        &self,
        records: &[LevelGameEntityRecord],
        index: usize,
        damage: u16,
        poise: u16,
    ) -> (u16, u16) {
        match (records.get(index), self.combatants.get(index)) {
            (Some(record), Some(combatant)) => {
                combatant.crit_scaled(&self.body(record).combat, damage, poise)
            }
            _ => (damage, poise),
        }
    }

    /// Whether a parry has armed a crit for entity `index`: an attack it
    /// starts now is the crit strike or crit shot.
    pub fn crit_armed(&self, index: usize) -> bool {
        self.combatants.get(index).is_some_and(|c| c.crit != 0)
    }

    /// Whether any entity has a crit armed (a cheap gate for the draw).
    pub fn any_crit_armed(&self) -> bool {
        self.combatants.iter().any(|c| c.crit != 0)
    }

    /// Whether entity `index`'s attack under way is a crit (it started
    /// while a crit was armed, [`Combatant::start_attack`]).
    pub fn attack_is_crit(&self, index: usize) -> bool {
        self.combatants.get(index).is_some_and(|c| c.crit_attack)
    }

    /// Use up entity `index`'s armed crit; true when one was armed.
    pub fn spend_crit(&mut self, index: usize) -> bool {
        self.combatants
            .get_mut(index)
            .is_some_and(Combatant::spend_crit)
    }

    /// Stagger entity `index` with the riposte of a parry by `by`
    /// ([`Strike::parry`]), through the same [`Self::receive_hit`] as any
    /// hit.
    pub fn riposte(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        by: CombatantId,
    ) -> GameEntityHitOutcome {
        let channel = self.stance(index);
        self.receive_hit(records, index, Strike::parry(by, channel))
    }

    /// Advance every live entity's swap cooldown and swap clock by `delta`
    /// ticks. The owner calls this once per simulation tick, after the
    /// entity tick, exactly as it advances the player's stance clock after
    /// her swap press: every fighter's swap window is then the same count of
    /// simulation ticks, whatever cadence the entities think at.
    /// [`Self::tick_delta_deferred`] leaves the clocks to this call;
    /// [`Self::tick_delta`] calls it itself.
    pub fn advance_stances(&mut self, delta: u16) {
        let count = self.count();
        let mut index = 0usize;
        while index < count {
            if self.state(index) != GameEntityState::Dead {
                self.combatants[index].advance_stance(delta);
            }
            index += 1;
        }
    }

    /// Ticks since the entity's latest stance change began; `u16::MAX` when
    /// it has not changed since spawning. The eye pulse is read off the same
    /// clock, so it needs no state of its own.
    fn stance_swap_elapsed(&self, index: usize) -> u16 {
        self.combatants[index].stance.swap_elapsed_ticks()
    }

    #[cfg(test)]
    fn advance_stance_swap(&mut self, index: usize, delta_ticks: u16) {
        self.combatants[index].stance.advance_swap(delta_ticks);
    }

    /// Swap entity `index`'s stance when its `record`'s moves allow a swap
    /// at all ([`CombatKits::can_swap`]) and its cooldown has run out: the
    /// one swap request every controller of an entity makes. True when it
    /// swapped.
    pub fn swap_stance(&mut self, record: &LevelGameEntityRecord, index: usize) -> bool {
        index < self.count()
            && Self::stance_moves(record).can_swap()
            && self.combatants[index].request_swap(self.swap_timing[index].swap_cooldown_ticks)
    }

    /// Whether [`Self::swap_stance`] would swap entity `index` now: the
    /// same moves rule and cooldown, asked without swapping. A controller
    /// asks before it plans what follows a swap.
    pub fn swap_permitted(&self, record: &LevelGameEntityRecord, index: usize) -> bool {
        self.holds_fighter(index)
            && Self::stance_moves(record).can_swap()
            && self.combatants[index].stance.can_swap()
    }

    /// Test hook: swap regardless of the moves rule.
    #[cfg(test)]
    fn mutate_stance(&mut self, index: usize) {
        self.combatants[index].request_swap(self.swap_timing[index].swap_cooldown_ticks);
    }

    fn capture_ranged_aim(&mut self, index: usize, input: GameEntityTickInput<'_>) {
        self.capture_ranged_target(index, input.target.position, input.target.height);
    }

    /// Where a shot at `input`'s target aims: its position raised to three
    /// quarters of its height.
    fn aim_point(input: GameEntityTickInput<'_>) -> [i32; 3] {
        Self::raised_aim(input.target.position, input.target.height)
    }

    fn raised_aim(position: [i32; 3], height: i32) -> [i32; 3] {
        [
            position[0],
            position[1].saturating_add(height.max(0).saturating_mul(3) / 4),
            position[2],
        ]
    }

    fn capture_ranged_target(&mut self, index: usize, player: [i32; 3], height: i32) {
        self.capture_ranged_point(index, Self::raised_aim(player, height));
    }

    /// Commit the aim of a shot at `point` (already raised to the aim
    /// height), along the entity's current bearing.
    fn capture_ranged_point(&mut self, index: usize, point: [i32; 3]) {
        let dx = point[0].saturating_sub(self.px(index));
        let dz = point[2].saturating_sub(self.pz(index));
        let distance =
            psx_math::int32::isqrt_i32(dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz)));
        let yaw = self.yaw_raw(index) as u16;
        // Retain the limited body turn during the tell. The aim point lies on
        // that bearing at the player's distance, rather than snapping sideways
        // or backwards if the player outruns the enemy's turn rate.
        self.ranged_aim_target[index] = [
            self.px(index).saturating_add(psx_math::int32::mul_q12_i32(
                distance,
                psx_math::sin_q12(yaw),
            )),
            point[1],
            self.pz(index).saturating_add(psx_math::int32::mul_q12_i32(
                distance,
                psx_math::cos_q12(yaw),
            )),
        ];
    }

    fn track_ranged_tell(&mut self, index: usize, input: GameEntityTickInput<'_>, delta: u16) {
        // Track only during the early tell. The final six ticks and the
        // complete active animation commit to that bearing: no input reading.
        if self.state_ticks[index].saturating_add(6) >= self.selected_timing(index).windup {
            return;
        }
        let dx = input.target.position[0].saturating_sub(self.px(index));
        let dz = input.target.position[2].saturating_sub(self.pz(index));
        if dx != 0 || dz != 0 {
            self.set_yaw(
                index,
                psx_engine::Angle::from_q12(self.yaw_raw(index) as u16)
                    .approach_q12(
                        psx_engine::Angle::from_q12(atan2_q12(dx, dz)),
                        32u16.saturating_mul(delta),
                    )
                    .as_q12() as i16,
            );
        }
        self.capture_ranged_aim(index, input);
    }

    /// The authored melee tell may turn slowly before its first active frame.
    /// Callers stop invoking this at the authored cutoff; contact and stagger
    /// also close it, so a combo never pivots after connecting.
    pub fn track_authored_melee_tell(&mut self, index: usize, player: [i32; 3], delta: u16) {
        if index >= self.count()
            || self.state(index) != GameEntityState::Attack
            || self.selected_attack_is_ranged(index)
            || self.authored_connection_mask[index] != 0
        {
            return;
        }
        let dx = player[0].saturating_sub(self.px(index));
        let dz = player[2].saturating_sub(self.pz(index));
        if dx != 0 || dz != 0 {
            self.set_yaw(
                index,
                psx_engine::Angle::from_q12(self.yaw_raw(index) as u16)
                    .approach_q12(
                        psx_engine::Angle::from_q12(atan2_q12(dx, dz)),
                        16u16.saturating_mul(delta),
                    )
                    .as_q12() as i16,
            );
        }
    }

    /// Track the visible charge until the caller's authored release cutoff.
    /// An interruption or a released shot closes tracking permanently for
    /// this attack. The spawned projectile keeps its captured velocity.
    pub fn track_authored_ranged_charge(
        &mut self,
        index: usize,
        player: [i32; 3],
        player_height: i32,
        delta: u16,
    ) {
        if index >= self.count()
            || self.state(index) != GameEntityState::Attack
            || !self.selected_attack_is_ranged(index)
            || self.authored_connection_mask[index] != 0
        {
            return;
        }
        let dx = player[0].saturating_sub(self.px(index));
        let dz = player[2].saturating_sub(self.pz(index));
        if dx != 0 || dz != 0 {
            self.set_yaw(
                index,
                psx_engine::Angle::from_q12(self.yaw_raw(index) as u16)
                    .approach_q12(
                        psx_engine::Angle::from_q12(atan2_q12(dx, dz)),
                        48u16.saturating_mul(delta),
                    )
                    .as_q12() as i16,
            );
        }
        self.capture_ranged_target(index, player, player_height);
    }

    /// Target captured before release. Moving after the charge cutoff can evade
    /// the shot: neither this query nor the projectile tracks the live player.
    pub fn ranged_target(&self, index: usize) -> Option<[i32; 3]> {
        if index >= self.count() {
            return None;
        }
        Some(self.ranged_aim_target[index])
    }

    /// Converge from the actual animated muzzle onto the committed aim point.
    /// A body-parallel ray misses by the cannon's full sideways/forward offset.
    pub fn ranged_velocity(&self, index: usize, muzzle: [i32; 3], speed: u16) -> [i32; 3] {
        let Some(target) = self.ranged_target(index) else {
            return [0; 3];
        };
        crate::projectiles::velocity_toward(muzzle, target, speed)
    }

    /// Clip selection for entity `index`'s current state. Locomotion
    /// states loop from their state-entry tick; the attack grammar
    /// plays the attack clip as ONE one-shot whose phase spans
    /// Windup + Attack + Recover (the telegraph/commit/punish the
    /// player reads is the same clip the AI windows run on). Stagger
    /// and Death are one-shots from state entry; Dead keeps counting
    /// ticks (see [`Self::tick`]) so the death clip finishes and
    /// holds its final frame as the corpse pose.
    pub fn clip_for_state(
        &self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
    ) -> GameEntityClip {
        self.state_anim(records, index).1
    }

    /// [`Self::clip_for_state`] and the animation action it plays, which
    /// decides how a change to or from it crossfades
    /// ([`crate::fighter_anim::blend_ticks`]).
    pub(super) fn state_anim(
        &self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
    ) -> (CharacterAnimationAction, GameEntityClip) {
        use CharacterAnimationAction as Action;
        if index >= self.count() || index >= records.len() {
            return (Action::Idle, GameEntityClip::default());
        }
        let record = &records[index];
        // An evade shows the clip of the evade the motor makes (roll,
        // backstep, dash left or right, by its side), as the player's does,
        // through the verb's fallback chain on its Character; the motor owns
        // the body until it ends.
        let motor = &self.motors[index];
        if !motor.action().is_idle() {
            let wanted = match self.motor_anims[index] {
                CharacterMotorAnim::DashLeft => Action::DashLeft,
                CharacterMotorAnim::DashRight => Action::DashRight,
                CharacterMotorAnim::Quickstep => Action::Backstep,
                _ => Action::Roll,
            };
            let (action, clip) = self
                .body(record)
                .verb_clip(wanted)
                .unwrap_or((Action::Roll, record.roll_clip));
            return (
                action,
                GameEntityClip {
                    clip,
                    phase_ticks: u16::from(motor.action_frame()),
                    one_shot: true,
                    speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
                    frame_range: CharacterActionFrameRange::FULL,
                },
            );
        }
        if let Some(shown) = self.tactical_clip(record, index) {
            return shown;
        }
        let ticks = self.state_ticks[index];
        let attack_clip = self.selected_attack_clip(record, index);
        let timing = self.selected_timing(index);
        let (attack_speed_q8, attack_frame_range) = self.selected_attack_playback(record, index);
        let looping = |clip: u16| GameEntityClip {
            clip,
            phase_ticks: ticks,
            one_shot: false,
            speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            frame_range: CharacterActionFrameRange::FULL,
        };
        let one_shot = |clip: u16, phase_ticks: u16| GameEntityClip {
            clip,
            phase_ticks,
            one_shot: true,
            speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            frame_range: CharacterActionFrameRange::FULL,
        };
        let attack_one_shot = |phase_ticks: u16| {
            (
                self.selected_attack_action(record, index),
                GameEntityClip {
                    clip: attack_clip,
                    phase_ticks,
                    one_shot: true,
                    speed_q8: attack_speed_q8,
                    frame_range: attack_frame_range,
                },
            )
        };
        let stagger = |ticks: u16| GameEntityClip {
            speed_q8: if record.stagger_speed_q8 == 0 {
                768
            } else {
                record.stagger_speed_q8
            },
            frame_range: record.stagger_frame_range,
            ..one_shot(record.stagger_clip, ticks)
        };
        match self.state(index) {
            GameEntityState::Idle => (Action::Idle, looping(record.idle_clip)),
            GameEntityState::Patrol => (Action::Walk, looping(record.walk_clip)),
            // The alert one-shot crossfades as a reaction does.
            GameEntityState::Aggro if ticks < u16::from(record.reaction_ticks) => {
                (Action::HitReact, one_shot(record.alert_clip, ticks))
            }
            GameEntityState::Aggro => match self.intent(index) {
                GameEntityIntent::Approach if Self::can_run(record) => {
                    (Action::Run, looping(Self::approach_clip(record)))
                }
                GameEntityIntent::Approach => (Action::Walk, looping(Self::approach_clip(record))),
                GameEntityIntent::CircleLeft => {
                    (Action::StrafeLeft, looping(record.strafe_left_clip))
                }
                GameEntityIntent::CircleRight => {
                    (Action::StrafeRight, looping(record.strafe_right_clip))
                }
                GameEntityIntent::Retreat => {
                    (Action::WalkBackward, looping(record.walk_backward_clip))
                }
                GameEntityIntent::Hold => (Action::Idle, looping(record.idle_clip)),
            },
            GameEntityState::Windup => attack_one_shot(ticks),
            GameEntityState::Attack => attack_one_shot(timing.windup.saturating_add(ticks)),
            GameEntityState::Recover => attack_one_shot(
                timing
                    .windup
                    .saturating_add(timing.active)
                    .saturating_add(ticks),
            ),
            // A parried attacker plays its Parried clip over the same
            // stagger; until one is authored that is the stagger clip.
            GameEntityState::Staggered
                if self.combat_flags[index] & GAME_ENTITY_PARRIED_STAGGER != 0
                    && record.parried_clip != record.stagger_clip =>
            {
                (Action::Parried, one_shot(record.parried_clip, ticks))
            }
            GameEntityState::Staggered => (Action::Stun, stagger(ticks)),
            // The knockdown clip plays once and holds its last frame while
            // the entity is down. Until a knockdown clip is authored it is the
            // stagger clip, at the stagger's authored playback.
            GameEntityState::KnockedDown if record.knockdown_clip == record.stagger_clip => {
                (Action::Knockdown, stagger(ticks))
            }
            GameEntityState::KnockedDown => {
                (Action::Knockdown, one_shot(record.knockdown_clip, ticks))
            }
            GameEntityState::Dead => (Action::Death, one_shot(record.death_clip, ticks)),
        }
    }

    /// Apply a hit to entity `index`: health damage on `channel` plus poise
    /// damage, both through the one stance rule over its body's
    /// [`CombatStats`](psx_level::CombatStats): a hit from the stance
    /// opposite the entity's active one takes the body's opposite-stance
    /// multipliers, a matching hit lands as authored. Accumulated poise
    /// damage past the body's capacity staggers.
    ///
    /// `channel` is the attack's vitality channel, which the player side
    /// derives from the swing (horizontal = Horizon, vertical = Zenith). The
    /// damage comes off the one health bar; the entity dies when it is empty.
    pub fn apply_hit(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        channel: VitalityChannelId,
        damage: u16,
        poise_damage: u16,
    ) -> GameEntityHitOutcome {
        let strike = player_strike(HitKind::Melee, channel, damage, poise_damage, false);
        self.receive_hit(records, index, strike)
    }

    /// Land `strike` on entity `index` through [`Combatant::receive_hit`]
    /// with the entity's record stats, then move the entity into Dead or
    /// Staggered when the hit killed or broke it. An out of range or dead
    /// entity is a miss.
    pub fn receive_hit(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        strike: Strike,
    ) -> GameEntityHitOutcome {
        if index >= self.count() || index >= records.len() {
            return GameEntityHitOutcome::MISS;
        }
        let state = self.state(index);
        if state == GameEntityState::Dead {
            return GameEntityHitOutcome::MISS;
        }
        let stats = self.body(&records[index]).combat;
        let poise = PoiseRule {
            armored: self.armoured(records, index),
            blocked: matches!(
                state,
                GameEntityState::Staggered | GameEntityState::KnockedDown
            ),
        };
        let out = self.combatants[index].receive_hit(HitIn {
            strike,
            stats,
            poise,
        });
        if out.parried {
            // An entity's attacker is staggered here; the owner staggers any
            // other attacker (the player) from the outcome.
            let attacker = usize::from(strike.attacker.0);
            if attacker < self.count() && attacker != index {
                self.riposte(records, attacker, CombatantId::entity(index));
            }
            // Nothing landed: not a connection, so no energy for the attacker.
            return GameEntityHitOutcome {
                parried: true,
                ..GameEntityHitOutcome::MISS
            };
        }
        let delay = u16::from(records[index].group_attack_delay_ticks);
        if out.killed {
            self.release_attack_owner(index, delay);
            self.enter_state(
                index,
                GameEntityState::Dead,
                &mut GameEntityTickStats::default(),
            );
            return GameEntityHitOutcome {
                connected: true,
                died: true,
                ..GameEntityHitOutcome::MISS
            };
        }
        if matches!(state, GameEntityState::Idle | GameEntityState::Patrol) && !out.poise_broken {
            self.notice_attacker(index, strike.attacker);
        }
        if out.poise_broken {
            self.release_attack_owner(index, delay);
            self.enter_state(
                index,
                if out.knocked_down {
                    GameEntityState::KnockedDown
                } else {
                    GameEntityState::Staggered
                },
                &mut GameEntityTickStats::default(),
            );
            if strike.kind == HitKind::Parry && !out.knocked_down {
                self.combat_flags[index] |= GAME_ENTITY_PARRIED_STAGGER;
            }
        }
        GameEntityHitOutcome {
            connected: true,
            staggered: out.poise_broken,
            knocked_down: out.knocked_down,
            ..GameEntityHitOutcome::MISS
        }
    }

    /// Whether entity `index`'s attack armours it now, by the one armour
    /// rule ([`crate::combatant::armoured`]): the attack's authored `Armored`
    /// window on its clip clock ([`crate::fighter_action::ActionTiming`]),
    /// else a heavy attack while its contact is live (the Attack state).
    // Asked once per landed hit: kept out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub fn armoured(&self, records: &[LevelGameEntityRecord], index: usize) -> bool {
        let Some(record) = records.get(index) else {
            return false;
        };
        let timing = self.selected_timing(index);
        crate::combatant::armoured(
            &self.body(record).combat,
            self.attack_since(index)
                .and_then(|since| timing.armoured_at(since)),
            self.state(index) == GameEntityState::Attack
                && self.selected_attack_kind(index) == GAME_ENTITY_ATTACK_HEAVY,
        )
    }

    /// Ticks since entity `index`'s attack started (windup, contact and
    /// recovery on its attack's clock); None outside an attack.
    fn attack_since(&self, index: usize) -> Option<u16> {
        let timing = self.selected_timing(index);
        let ticks = self.state_ticks[index];
        match self.state(index) {
            GameEntityState::Windup => Some(ticks),
            GameEntityState::Attack => Some(timing.windup.saturating_add(ticks)),
            GameEntityState::Recover => Some(
                timing
                    .windup
                    .saturating_add(timing.active)
                    .saturating_add(ticks),
            ),
            _ => None,
        }
    }

    /// Sweep the player's melee arc over the live entities: every
    /// enemy in the arc's room whose hurtbox cylinder the arc reaches
    /// (and whose bit in `already_hit` is clear) takes one
    /// [`Self::apply_hit`], and its bit latches so one swing connects
    /// at most once per enemy. An entity inside its i-frames
    /// ([`Self::invulnerable`]) is passed through without latching.
    /// `O(live entities)` with the per-axis/squared early-outs of
    /// [`arc_hits_circle`]; the owning
    /// game clears `already_hit` when a new swing starts. Bit `i`
    /// tracks entity `i` (the [`psx_level::MAX_GAME_ENTITY_RECORDS`]
    /// = 64 contract cap is exactly the u64 width).
    pub fn apply_melee_arc(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        arc: &MeleeArc,
        channel: VitalityChannelId,
        damage: u16,
        poise_damage: u16,
        // psx-numeric-allow-next-line: one-hit-per-swing bitmask; bit ops only, two-word on R3000
        already_hit: &mut u64,
    ) -> MeleeArcStats {
        self.apply_melee_arc_occluded(
            records,
            arc,
            channel,
            damage,
            poise_damage,
            false,
            already_hit,
            |_, _| false,
        )
    }

    /// [`Self::apply_melee_arc`] with a caller-supplied world occlusion test.
    /// `occluded(entity, position)` returning true blocks the connection
    /// WITHOUT latching the swing bit, so a target revealed later in the same
    /// active window (a door finishing its travel) can still be hit once.
    /// `crit` marks the swing as a crit (its posture lands on a riposted
    /// fighter, see [`Strike::crit`]).
    pub fn apply_melee_arc_occluded(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        arc: &MeleeArc,
        channel: VitalityChannelId,
        damage: u16,
        poise_damage: u16,
        crit: bool,
        // psx-numeric-allow-next-line: one-hit-per-swing bitmask; bit ops only, two-word on R3000
        already_hit: &mut u64,
        mut occluded: impl FnMut(usize, [i32; 3]) -> bool,
    ) -> MeleeArcStats {
        // psx-numeric-allow-next-line: the 64-record cap IS the mask width
        // The records stop at 64 (`psx_level::MAX_GAME_ENTITY_RECORDS`); a 65th
        // slot can only be the actor slot, which no swing walks.
        const { assert!(MAX_ENTITIES <= 65, "swing mask is a u64") };
        let mut stats = MeleeArcStats::default();
        let count = self.count().min(records.len());
        let mut entity = 0usize;
        while entity < count {
            let record = &records[entity];
            // psx-numeric-allow-next-line: swing bitmask bit select; no 64-bit arithmetic
            let mask = 1u64 << entity;
            let skip = *already_hit & mask != 0
                || record.room != arc.room
                || self.state(entity) == GameEntityState::Dead
                || self.passes_through(records, entity)
                || !arc_hits_circle(
                    arc,
                    self.px(entity),
                    self.pz(entity),
                    i32::from(self.body(record).radius),
                )
                || occluded(entity, self.position(entity));
            if !skip {
                *already_hit |= mask;
                let outcome = self.receive_hit(
                    records,
                    entity,
                    player_strike(HitKind::Melee, channel, damage, poise_damage, crit),
                );
                stats.hits += u16::from(outcome.connected);
                stats.staggers += u16::from(outcome.staggered);
                stats.deaths += u16::from(outcome.died);
                stats.parries += u16::from(outcome.parried);
            }
            entity += 1;
        }
        stats
    }

    /// Advance every entity one 60 Hz tick. Thinking is gated on the
    /// owner's spatial mask hl-psx-style: an entity outside it only
    /// thinks while its behavior is engaged (anything past
    /// Idle/Patrol) or the player is inside its notice range.
    pub fn tick(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
    ) -> GameEntityTickStats {
        self.tick_delta(records, input, mover, 1)
    }

    /// Advance entity behaviour by `delta_ticks` 60 Hz ticks in one pass.
    /// Movement and state clocks scale by the same delta, allowing games that
    /// render at 30 Hz to run collision-heavy NPC thinking at visual cadence
    /// without halving movement speed or animation phase.
    pub fn tick_delta(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        delta_ticks: u16,
    ) -> GameEntityTickStats {
        let stats = self.tick_delta_impl::<0>(records, input, mover, delta_ticks, None);
        self.advance_stances(delta_ticks.max(1));
        stats
    }

    /// Advance entity behaviour while deferring attack contact to the owner of
    /// the retained actor poses.
    ///
    /// Every active attack writes one [`DeferredGameEntityAttack`] containing
    /// the exact attack clip/phase and transform sampled before any
    /// Attack-to-Recover transition. The caller resolves body/equipment poses,
    /// evaluates authored combat capsules, and latches a connection with
    /// [`Self::connect_deferred_melee_window`] (or the legacy one-hit API).
    /// `player_hits` and `player_damage` in
    /// the returned stats are therefore zero; the caller reports those after
    /// pose-backed contact resolution.
    pub fn tick_delta_deferred<const MAX_ATTACKS: usize>(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        delta_ticks: u16,
        attacks: &mut DeferredGameEntityAttacks<MAX_ATTACKS>,
    ) -> GameEntityTickStats {
        attacks.clear();
        self.tick_delta_impl(records, input, mover, delta_ticks, Some(attacks))
    }

    fn tick_delta_impl<const MAX_ATTACKS: usize>(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        shared: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        delta_ticks: u16,
        mut deferred: Option<&mut DeferredGameEntityAttacks<MAX_ATTACKS>>,
    ) -> GameEntityTickStats {
        let delta_ticks = delta_ticks.max(1);
        self.attack_tick_generation = self.attack_tick_generation.wrapping_add(1);
        let mut stats = GameEntityTickStats::default();
        let count = self.count().min(records.len());
        self.update_combat_director(records, shared, delta_ticks, &mut stats);
        self.speed_scale = mover.speed_scale();
        let mut index = 0usize;
        while index < count {
            let record = &records[index];
            // An entity with a rival engages that slot as it stands before
            // this entity thinks; the rest engage the owner's target.
            let rival_fighter;
            let input = match self.live_rival(index, count) {
                Some(rival) => {
                    rival_fighter = self.combatants[rival];
                    GameEntityTickInput {
                        target: self.rival_target(&records[rival], rival, &rival_fighter),
                    }
                }
                None => shared,
            };
            let state = GameEntityState::from_raw(self.state[index]);
            self.motion = NO_MOTION;
            self.probing = None;
            self.driven = false;
            if state == GameEntityState::Dead {
                // Dead entities stop thinking but keep counting so
                // the death one-shot plays out and then holds its
                // final frame (see clip_for_state).
                self.state_ticks[index] = self.state_ticks[index].saturating_add(delta_ticks);
                index += 1;
                continue;
            }
            self.advance_controller_clocks(index, delta_ticks);
            let swapping = self.stance_swap_in_progress(index);
            self.combatants[index].tick(
                delta_ticks,
                matches!(
                    state,
                    GameEntityState::Staggered | GameEntityState::KnockedDown
                ),
                swapping,
            );
            self.step_recoil(record, index, mover, delta_ticks);
            let behavior_awake = !matches!(state, GameEntityState::Idle | GameEntityState::Patrol);
            let spatially_active = index < 64 && self.spatial_active_mask & (1u64 << index) != 0;
            // PVS is a rendering broad phase, not a perception verdict.
            // Nearby actors must still test sight/hearing around corners.
            let activation_allows =
                spatially_active || self.target_in_notice_range(record, index, input);
            if !behavior_awake && !activation_allows {
                stats.gated += 1;
                index += 1;
                continue;
            }
            stats.thought += 1;
            self.state_ticks[index] = self.state_ticks[index].saturating_add(delta_ticks);
            match state {
                GameEntityState::Idle => self.tick_idle(record, index, input, mover, &mut stats),
                GameEntityState::Patrol => {
                    self.tick_patrol(record, index, input, mover, delta_ticks, &mut stats)
                }
                GameEntityState::Aggro => {
                    self.tick_aggro(record, index, input, mover, delta_ticks, &mut stats)
                }
                GameEntityState::Windup => {
                    self.step_firing(record, index, input, mover, delta_ticks);
                    self.step_melee_tell(record, index, input, mover, delta_ticks);
                    if self.selected_attack_is_ranged(index) {
                        self.track_ranged_tell(index, input, delta_ticks);
                    }
                    if self.state_ticks[index] >= self.selected_timing(index).windup {
                        self.enter_state(index, GameEntityState::Attack, &mut stats);
                    }
                }
                GameEntityState::Attack => {
                    self.step_firing(record, index, input, mover, delta_ticks);
                    stats.attacking += 1;
                    match deferred.as_deref_mut() {
                        Some(attacks) => attacks.push(self.deferred_attack(record, index)),
                        None => self.resolve_attack_contact(record, index, input, &mut stats),
                    }
                    if self.state_ticks[index] >= self.selected_timing(index).active {
                        self.enter_state(index, GameEntityState::Recover, &mut stats);
                    }
                }
                GameEntityState::Recover => {
                    self.step_firing(record, index, input, mover, delta_ticks);
                    if self.state_ticks[index] >= self.selected_timing(index).recovery {
                        self.attack_cooldown[index] = u16::from(record.attack_cooldown_ticks);
                        self.release_attack_owner(
                            index,
                            u16::from(record.group_attack_delay_ticks),
                        );
                        self.enter_state(index, GameEntityState::Aggro, &mut stats);
                        // The acquisition reaction belongs to first notice;
                        // recovery already supplied this attack's pause.
                        self.state_ticks[index] = u16::from(record.reaction_ticks);
                        self.choose_spacing(record, index, input);
                    }
                }
                GameEntityState::Staggered => {
                    if self.state_ticks[index]
                        >= if record.stagger_ticks == 0 {
                            GAME_ENTITY_STAGGER_TICKS
                        } else {
                            record.stagger_ticks
                        }
                    {
                        self.enter_state(index, GameEntityState::Aggro, &mut stats);
                        // The authored reaction is for first acquisition, not
                        // an extra pause after the player already won a stagger.
                        self.state_ticks[index] = u16::from(record.reaction_ticks);
                    }
                }
                GameEntityState::KnockedDown => {
                    if !self.combatants[index].knocked_down() {
                        self.enter_state(index, GameEntityState::Aggro, &mut stats);
                        // Getting up is not a new acquisition.
                        self.state_ticks[index] = u16::from(record.reaction_ticks);
                    }
                }
                GameEntityState::Dead => {}
            }
            if !self.driven {
                self.drive(record, index, mover, delta_ticks);
            }
            index += 1;
        }
        stats
    }

    fn deferred_attack(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
    ) -> DeferredGameEntityAttack {
        let action = self.selected_attack_action(record, index);
        let ranged = self.selected_attack_is_ranged(index);
        let (speed_q8, frame_range) = self.selected_attack_playback(record, index);
        DeferredGameEntityAttack {
            entity: index.min(u16::MAX as usize) as u16,
            tick_generation: self.attack_tick_generation,
            swing_sequence: self.attack_sequence[index],
            clip: GameEntityClip {
                clip: self.selected_attack_clip(record, index),
                phase_ticks: self
                    .selected_timing(index)
                    .windup
                    .saturating_add(self.state_ticks[index]),
                one_shot: true,
                speed_q8,
                frame_range,
            },
            action,
            ranged,
            position: self.position(index),
            yaw: self.yaw_raw(index) as u16,
            room: record.room,
        }
    }

    /// Whether `attack` still names this tick and has not connected during its
    /// current swing. Invalid/stale tokens fail closed.
    pub fn deferred_attack_can_connect(&self, attack: DeferredGameEntityAttack) -> bool {
        let index = attack.entity();
        index < self.count()
            && attack.tick_generation == self.attack_tick_generation
            && attack.swing_sequence == self.attack_sequence[index]
            && self.combat_flags[index] & GAME_ENTITY_ATTACK_CONNECTED == 0
    }

    /// Per-emitter release latches for a valid ranged attack. A stagger, death,
    /// new swing or new entity tick invalidates the old token.
    pub fn deferred_projectile_release_mask(
        &self,
        attack: DeferredGameEntityAttack,
    ) -> Option<u16> {
        (attack.is_ranged() && self.deferred_attack_can_connect(attack))
            .then(|| self.authored_connection_mask[attack.entity()])
    }

    /// Consume only this emitter after spawning its shot. Other authored
    /// release markers remain available, but no marker can fire twice.
    pub fn commit_deferred_projectile(
        &mut self,
        attack: DeferredGameEntityAttack,
        emitter: u8,
    ) -> bool {
        let Some(mask) = self.deferred_projectile_release_mask(attack) else {
            return false;
        };
        if emitter >= 16 || mask & (1u16 << emitter) != 0 {
            return false;
        }
        self.authored_connection_mask[attack.entity()] = mask | (1u16 << emitter);
        true
    }

    /// Per-window melee contacts, sharing storage with ranged emitter latches.
    pub fn deferred_melee_hit_mask(&self, attack: DeferredGameEntityAttack) -> Option<u16> {
        (!attack.is_ranged() && self.deferred_attack_can_connect(attack))
            .then(|| self.authored_connection_mask[attack.entity()])
    }

    /// Consume all simultaneously active hitboxes as one verified contact.
    /// Later non-overlapping windows remain available in the same animation.
    pub fn connect_deferred_melee_window(
        &mut self,
        attack: DeferredGameEntityAttack,
        window: u16,
    ) -> bool {
        let Some(used) = self.deferred_melee_hit_mask(attack) else {
            return false;
        };
        if window == 0 || used & window != 0 {
            return false;
        }
        self.authored_connection_mask[attack.entity()] = used | window;
        true
    }

    /// Test the explicitly legacy Character-radius/front-arc contact policy for
    /// a deferred token. Games should call this only when authored attacker
    /// hitboxes or defender hurtboxes are absent; an authored inactive frame or
    /// authored geometric miss is authoritative and must not fall back here.
    pub fn deferred_attack_legacy_arc_hits(
        &self,
        records: &[LevelGameEntityRecord],
        attack: DeferredGameEntityAttack,
        player: [i32; 3],
        player_radius: i32,
    ) -> bool {
        let index = attack.entity();
        if !self.deferred_attack_can_connect(attack) || records.get(index).is_none() {
            return false;
        }
        let record = &records[index];
        if attack.is_ranged() {
            return false;
        }
        let arc = MeleeArc {
            room: attack.room,
            x: attack.position[0],
            z: attack.position[2],
            yaw: attack.yaw,
            reach: i32::from(self.body(record).radius)
                .saturating_add(player_radius.max(0))
                .saturating_add(GAME_ENTITY_ATTACK_REACH_MARGIN),
            half_angle: GAME_ENTITY_ATTACK_HALF_ANGLE,
        };
        arc_hits_circle(&arc, player[0], player[2], 0)
    }

    /// Latch one verified deferred contact. Returns `false` for stale tokens or
    /// a swing that already connected, preserving one-hit-per-swing even if a
    /// caller accidentally evaluates the same frame twice.
    pub fn connect_deferred_attack(&mut self, attack: DeferredGameEntityAttack) -> bool {
        self.commit_deferred_attack(attack)
    }

    /// Consume one verified deferred attack after its committed effect is
    /// created. Melee calls this on contact; ranged attacks call it after a
    /// projectile successfully enters the fixed pool. Pool overflow therefore
    /// leaves the release retryable for the rest of its authored frame window.
    pub fn commit_deferred_attack(&mut self, attack: DeferredGameEntityAttack) -> bool {
        if !self.deferred_attack_can_connect(attack) {
            return false;
        }
        self.combat_flags[attack.entity()] |= GAME_ENTITY_ATTACK_CONNECTED;
        true
    }

    /// Update cooldown clocks and grant the single shared attack slot. The
    /// fixed-array scan is deliberately simple: waiting time prevents
    /// starvation, authored priority distinguishes archetypes, and distance
    /// breaks ties in favour of an enemy already presenting a readable threat.
    fn update_combat_director(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        input: GameEntityTickInput<'_>,
        delta_ticks: u16,
        stats: &mut GameEntityTickStats,
    ) {
        self.director_delay_ticks = self.director_delay_ticks.saturating_sub(delta_ticks);
        let count = self.count().min(records.len());

        if let Some(owner) = self.attack_owner() {
            let state = self.state(owner);
            if state == GameEntityState::Aggro {
                self.attack_owner_chase_ticks =
                    self.attack_owner_chase_ticks.saturating_add(delta_ticks);
                if self.attack_owner_chase_ticks >= 120 {
                    // An obstructed pursuer must not starve the rest of the
                    // group. Yield, reposition and retry after a short delay.
                    self.attack_cooldown[owner] = self.attack_cooldown[owner].max(60);
                    self.release_attack_owner(owner, 0);
                }
            } else {
                self.attack_owner_chase_ticks = 0;
            }
            if !matches!(
                state,
                GameEntityState::Aggro
                    | GameEntityState::Windup
                    | GameEntityState::Attack
                    | GameEntityState::Recover
            ) {
                self.release_attack_owner(
                    owner,
                    u16::from(records[owner].group_attack_delay_ticks),
                );
            }
        }

        let mut index = 0usize;
        while index < count {
            self.attack_cooldown[index] = self.attack_cooldown[index].saturating_sub(delta_ticks);
            if self.state(index) == GameEntityState::Aggro && self.attack_owner() != Some(index) {
                self.attack_wait_ticks[index] =
                    self.attack_wait_ticks[index].saturating_add(delta_ticks);
            }
            index += 1;
        }

        if self.attack_owner().is_some() || self.director_delay_ticks != 0 {
            return;
        }

        let mut selected = None;
        let mut best_score = i32::MIN;
        index = 0;
        while index < count {
            let record = &records[index];
            let state = self.state(index);
            let ready = state == GameEntityState::Aggro
                && self.rivals[index] == NO_RIVAL
                // A dry cannon must not monopolize the shared attack slot
                // while another enemy has a legal attack available.
                && (self.stance(index) == VitalityChannelId::One
                    || self.can_fire_energy(record, index))
                && self.tactical_attack_ready(index)
                && self.attack_cooldown[index] == 0
                && self.state_ticks[index] >= u16::from(record.reaction_ticks)
                && self.target_within(
                    index,
                    input,
                    i32::from(record.preferred_distance)
                        .max(self.attack_reach(record, input))
                        .saturating_add(i32::from(record.spacing_tolerance)),
                );
            if ready {
                let dx = self
                    .px(index)
                    .saturating_sub(input.target.position[0])
                    .abs();
                let dz = self
                    .pz(index)
                    .saturating_sub(input.target.position[2])
                    .abs();
                let distance_penalty = dx.max(dz) >> 4;
                let score = i32::from(self.attack_wait_ticks[index])
                    .saturating_add(i32::from(record.attack_priority) * 64)
                    .saturating_sub(distance_penalty);
                if score > best_score {
                    best_score = score;
                    selected = Some(index);
                }
            }
            index += 1;
        }
        if let Some(index) = selected {
            self.attack_owner_plus_one = (index + 1) as u16;
            self.attack_owner_chase_ticks = 0;
            self.attack_wait_ticks[index] = 0;
            self.set_intent(index, GameEntityIntent::Approach);
            stats.attack_grants = stats.attack_grants.saturating_add(1);
        }
    }

    /// Take the shared attack slot for `index` unless another entity holds
    /// it, without the director's wait: a riposte is a reflex. True when
    /// `index` holds the slot.
    fn claim_attack_owner(&mut self, index: usize) -> bool {
        if self.rivals[index] != NO_RIVAL {
            return true;
        }
        match self.attack_owner() {
            Some(owner) => owner == index,
            None => {
                self.attack_owner_plus_one = (index + 1) as u16;
                self.attack_owner_chase_ticks = 0;
                self.attack_wait_ticks[index] = 0;
                true
            }
        }
    }

    /// Whether entity `index` may start an attack: a fighter with a rival
    /// always holds its own slot (it is the only one fighting that rival);
    /// the others take turns with the shared slot the director grants.
    fn holds_attack(&self, index: usize) -> bool {
        self.rivals[index] != NO_RIVAL || self.attack_owner() == Some(index)
    }

    fn release_attack_owner(&mut self, index: usize, delay_ticks: u16) {
        if self.attack_owner() == Some(index) {
            self.attack_owner_plus_one = 0;
            self.director_delay_ticks = self.director_delay_ticks.max(delay_ticks);
        }
    }

    fn set_intent(&mut self, index: usize, intent: GameEntityIntent) {
        self.intent[index] = intent as u8;
    }

    fn enter_state(
        &mut self,
        index: usize,
        state: GameEntityState,
        stats: &mut GameEntityTickStats,
    ) {
        if matches!(
            state,
            GameEntityState::Staggered | GameEntityState::KnockedDown | GameEntityState::Dead
        ) {
            self.finish_goal(index, EnemyGoalResult::Cancelled);
        }
        self.state[index] = state as u8;
        self.state_ticks[index] = 0;
        self.combat_flags[index] &= !GAME_ENTITY_PARRIED_STAGGER;
        if matches!(
            state,
            GameEntityState::Staggered | GameEntityState::KnockedDown | GameEntityState::Dead
        ) {
            // A pose retained earlier this tick cannot deal damage after its
            // owner was interrupted, even on the final Attack -> Recover tick.
            self.attack_sequence[index] = self.attack_sequence[index].wrapping_add(1);
            // A reaction owns the body: a roll in progress stops, as the
            // player's does.
            self.motors[index].interrupt_action();
        }
        self.move_yaw_valid[index] = 0;
        self.move_tried[index] = 0;
        if state != GameEntityState::Aggro {
            self.set_intent(index, GameEntityIntent::Hold);
        }
        if matches!(state, GameEntityState::Idle | GameEntityState::Dead) {
            self.attack_wait_ticks[index] = 0;
        }
        match state {
            GameEntityState::Patrol => stats.patrol_enters += 1,
            GameEntityState::Aggro => stats.aggro_enters += 1,
            GameEntityState::Windup => {
                stats.windup_enters += 1;
                // Every attack starts here: it is the crit strike or shot
                // when a crit is armed now.
                self.combatants[index].start_attack();
            }
            GameEntityState::Attack => {
                stats.attack_enters += 1;
                if self.selected_attack_is_ranged(index) {
                    stats.ranged_attack_enters += 1;
                } else {
                    stats.melee_attack_enters += 1;
                }
                // A fresh animation gets independent authored melee-window or
                // projectile-emitter latches. Legacy arcs still connect once.
                self.authored_connection_mask[index] = 0;
                self.combat_flags[index] &= !GAME_ENTITY_ATTACK_CONNECTED;
                self.attack_sequence[index] = self.attack_sequence[index].wrapping_add(1);
            }
            _ => {}
        }
    }

    /// One Attack-window contact test against the player: the same
    /// Character-derived reach that committed the windup, gated to a
    /// front arc around the facing the entity locked when it
    /// committed. Connects at most once per swing, and never against
    /// an i-framing player (see [`CombatTarget::defender`]).
    fn resolve_attack_contact(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        stats: &mut GameEntityTickStats,
    ) {
        // Contact on a rival resolves only through the deferred tokens,
        // on both fighters' poses (the owner's resolver).
        if self.selected_attack_is_ranged(index)
            || self.rivals[index] != NO_RIVAL
            || self.combat_flags[index] & GAME_ENTITY_ATTACK_CONNECTED != 0
            || input.target.defender.invulnerable()
        {
            return;
        }
        let arc = MeleeArc {
            room: record.room,
            x: self.px(index),
            z: self.pz(index),
            yaw: self.yaw_raw(index) as u16,
            reach: self.melee_attack_reach(record, input),
            half_angle: GAME_ENTITY_ATTACK_HALF_ANGLE,
        };
        // The player hurtbox center is the motor position; its radius
        // is already inside `attack_reach` (radius + radius + margin),
        // so the arc tests the CENTER (radius 0) to avoid counting the
        // player capsule twice.
        if !arc_hits_circle(&arc, input.target.position[0], input.target.position[2], 0) {
            return;
        }
        self.combat_flags[index] |= GAME_ENTITY_ATTACK_CONNECTED;
        stats.player_hits += 1;
        stats.player_damage = stats.player_damage.saturating_add(record.touch_damage);
    }

    fn target_within(&self, index: usize, input: GameEntityTickInput<'_>, radius: i32) -> bool {
        within_xz(
            [self.px(index), self.pz(index)],
            [input.target.position[0], input.target.position[2]],
            radius,
        )
    }

    /// Attack-band outer edge. Ranged attacks use their authored maximum;
    /// melee uses both body radii plus the close-in margin.
    fn attack_reach(&self, record: &LevelGameEntityRecord, input: GameEntityTickInput<'_>) -> i32 {
        if Self::has_ranged_attack(record) {
            i32::from(record.attack_max_range)
        } else {
            i32::from(self.body(record).radius)
                .saturating_add(input.target.radius.max(0))
                .saturating_add(GAME_ENTITY_ATTACK_REACH_MARGIN)
        }
    }

    fn melee_attack_reach(
        &self,
        record: &LevelGameEntityRecord,
        input: GameEntityTickInput<'_>,
    ) -> i32 {
        i32::from(self.body(record).radius)
            .saturating_add(input.target.radius.max(0))
            .saturating_add(GAME_ENTITY_ATTACK_REACH_MARGIN)
    }

    fn target_in_notice_range(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
    ) -> bool {
        psx_math::int32::abs_i32(self.py(index).saturating_sub(input.target.position[1]))
            <= i32::from(self.body(record).height)
                .max(input.target.height)
                .saturating_mul(2)
            && self.target_within(index, input, i32::from(record.aggro_radius))
    }

    // Shared by idle and patrol; keep one copy in the RAM-limited guest.
    #[inline(never)]
    fn target_noticed(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
    ) -> bool {
        if !within_xz(
            [record.x, record.z],
            [input.target.position[0], input.target.position[2]],
            i32::from(record.aggro_radius),
        ) || self.tactics[index].home_cooldown != 0
        {
            return false;
        }
        if !self.target_in_notice_range(record, index, input) {
            return false;
        }
        let clear = self.target_in_line_of_sight(record, index, input, mover);
        // Omni-directional short-range hearing, attenuated through cover.
        // Hearing wakes the normal alert/pursuit state; it never bypasses the
        // separate line-of-sight check required to commit an attack.
        let hearing = if clear {
            input.target.noise_radius
        } else {
            input.target.noise_radius / 3
        };
        if hearing > 0 && self.target_within(index, input, hearing) {
            return true;
        }
        if !clear {
            return false;
        }
        // Close peripheral awareness prevents touching an idle enemy unseen.
        let close = i32::from(self.body(record).height).max(input.target.height);
        if self.target_within(index, input, close) {
            return true;
        }
        let dx = input.target.position[0].saturating_sub(self.px(index));
        let dz = input.target.position[2].saturating_sub(self.pz(index));
        let yaw = psx_engine::Angle::from_q12(self.yaw_raw(index) as u16);
        yaw.sin().mul_i32(dx).saturating_add(yaw.cos().mul_i32(dz)) >= 0
    }

    fn target_in_line_of_sight(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
    ) -> bool {
        let from = [
            self.px(index),
            self.py(index)
                .saturating_add(i32::from(self.body(record).height) / 2),
            self.pz(index),
        ];
        let to = [
            input.target.position[0],
            input.target.position[1].saturating_add(input.target.height.max(1) / 2),
            input.target.position[2],
        ];
        mover.line_of_sight(record.room, from, to)
    }

    fn tick_idle(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        stats: &mut GameEntityTickStats,
    ) {
        if self.target_noticed(record, index, input, mover) {
            self.remember_target(index, input.target.position);
            self.enter_state(index, GameEntityState::Aggro, stats);
            return;
        }
        let has_patrol = record.patrol_x != record.x
            || record.patrol_y != record.y
            || record.patrol_z != record.z;
        if has_patrol && self.state_ticks[index] >= record.patrol_wait_ticks {
            self.enter_state(index, GameEntityState::Patrol, stats);
        }
    }

    fn tick_patrol(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        delta_ticks: u16,
        stats: &mut GameEntityTickStats,
    ) {
        if self.target_noticed(record, index, input, mover) {
            self.remember_target(index, input.target.position);
            self.enter_state(index, GameEntityState::Aggro, stats);
            return;
        }
        let goal = if self.patrol_leg[index] == 0 {
            [record.patrol_x, record.patrol_y, record.patrol_z]
        } else {
            [record.x, record.y, record.z]
        };
        let speed = self
            .walk_units(record)
            .saturating_mul(i32::from(delta_ticks));
        if self.step_toward(record, index, goal, speed, delta_ticks) {
            self.patrol_leg[index] ^= 1;
            self.enter_state(index, GameEntityState::Idle, stats);
        }
    }

    fn tick_aggro(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        delta_ticks: u16,
        stats: &mut GameEntityTickStats,
    ) {
        self.tick_tactical(record, index, input, mover, delta_ticks, stats);
    }

    // ------------------------------------------------------------------
    // The one mover. A controller writes `self.motion` (where to go, which
    // way to face, run, roll); `drive` spends it on the entity's motor once
    // per tick, through the same motor update the player's character gets.
    // ------------------------------------------------------------------

    fn px(&self, index: usize) -> i32 {
        self.motors[index].position().x
    }

    fn py(&self, index: usize) -> i32 {
        self.motors[index].position().y
    }

    fn pz(&self, index: usize) -> i32 {
        self.motors[index].position().z
    }

    /// Facing of entity `index`, PSX angle units.
    fn yaw_raw(&self, index: usize) -> i16 {
        self.motors[index].yaw().as_q12() as i16
    }

    /// Turn entity `index` to `yaw` at once (a planted pivot or an attack
    /// squaring up; walking turns go through the motor).
    fn set_yaw(&mut self, index: usize, yaw: i16) {
        self.motors[index].face(Angle::from_q12(yaw as u16));
    }

    /// Move entity `index` to `position` without walking there (a knockback
    /// push or a reset), keeping its facing, stamina and any action.
    fn place(&mut self, index: usize, position: [i32; 3]) {
        self.motors[index].relocate(RoomPoint::new(position[0], position[1], position[2]));
    }

    /// Set one axis (0 x, 1 y, 2 z) of entity `index`'s position: a test
    /// fixture's teleport.
    #[cfg(test)]
    pub(crate) fn set_axis(&mut self, index: usize, axis: usize, value: i32) {
        let mut p = self.position(index);
        p[axis] = value;
        self.place(index, p);
    }

    /// The motor configuration of `record`'s character: its own motion
    /// numbers under the game's speed scale, built by the same function as
    /// the player's ([`crate::character::motion_motor_config`]).
    // Cold AI-side helper: one small copy on the PS1 instead of one per caller.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn motor_config(&self, record: &LevelGameEntityRecord) -> CharacterMotorConfig {
        crate::character::motion_motor_config(
            i32::from(self.body(record).radius),
            i32::from(self.body(record).height),
            &self.body(record).motion(),
            Self::can_run(record),
            self.speed_scale,
        )
    }

    /// The dodge `record`'s character has, for its ranged exchange to plan
    /// with: its roll with the mobility kit (the roll's reach and frames),
    /// otherwise a walk aside over the same frames.
    // Cold AI-side helper: one small copy on the PS1 instead of one per caller.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn evade_move(&self, record: &LevelGameEntityRecord) -> crate::ranged_tactics::EvadeMove {
        crate::ranged_tactics::EvadeMove::of(
            &self.motor_config(record),
            crate::combat_intent::CombatAction::Evade.permitted(Self::stance_moves(record)),
        )
    }

    /// Pace of `record`'s character in Q8 engine units per frame: its walk,
    /// or its run when `run`.
    fn pace_q8(&self, record: &LevelGameEntityRecord, run: bool) -> i32 {
        let speed = if run && Self::can_run(record) {
            self.body(record).run_speed
        } else {
            self.body(record).walk_speed
        };
        crate::character::scaled_motion_speed(speed, self.speed_scale).max(1)
    }

    /// Whole engine units `record`'s character covers per tick at a walk.
    fn walk_units(&self, record: &LevelGameEntityRecord) -> i32 {
        (self.pace_q8(record, false) >> 8).max(1)
    }

    /// Whole engine units `record`'s character covers per tick at a run.
    fn run_units(&self, record: &LevelGameEntityRecord) -> i32 {
        (self.pace_q8(record, true) >> 8).max(1)
    }

    /// The move that covers `(dx, dz)` in `delta` frames at `record`'s walk
    /// (or run, when `run`): its direction, at the share of the pace that
    /// distance needs, full pace at most. The same world-space stick a pad
    /// gives, so the motor cannot tell who asked.
    // Cold AI-side helper: one small copy on the PS1 instead of one per caller.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn move_toward(
        &self,
        record: &LevelGameEntityRecord,
        dx: i32,
        dz: i32,
        delta: u16,
        run: bool,
    ) -> CombatMove {
        if dx == 0 && dz == 0 {
            return CombatMove::Still;
        }
        let mut x = dx;
        let mut z = dz;
        let mut far = false;
        while x.unsigned_abs() >= 1 << 14 || z.unsigned_abs() >= 1 << 14 {
            x >>= 1;
            z >>= 1;
            far = true;
        }
        // Unscaled, `length` is the distance; scaled, the distance is beyond
        // any pace.
        let length = psx_math::int32::isqrt_i32(x * x + z * z).max(1);
        let distance = length;
        let capacity = self
            .pace_q8(record, run)
            .saturating_mul(i32::from(delta.max(1)));
        // Under one pace the share is distance / capacity; both sides stay
        // small there, so the Q12 product cannot overflow.
        let share = if far || distance.saturating_mul(256) >= capacity {
            4096
        } else {
            (distance * 256 * 4096 / capacity).max(1)
        };
        let strength = Self::tilt_for_share(share);
        CombatMove::Move {
            x: (x * strength / length) as i16,
            z: (z * strength / length) as i16,
            forward: 0,
        }
    }

    /// Write a controller's move into the motor input for this tick:
    /// `face` keeps the body turned toward that yaw (the motor turns it at
    /// the character's own rate), None turns it into the move.
    fn request(&mut self, movement: CombatMove, face: Option<Angle>, run: bool) {
        if let CombatMove::Move { x, z, forward } = movement {
            self.motion.move_x = psx_engine::Q12::from_raw(i32::from(x));
            self.motion.move_z = psx_engine::Q12::from_raw(i32::from(z));
            self.motion.walk = forward;
        }
        self.motion.facing_yaw = face;
        self.motion.sprint = run;
    }

    /// The stick tilt (Q12) that moves a body at `share` (Q12) of its pace.
    /// The motor's response to a stick is quadratic (the tilt scales both
    /// the direction and the pace, as a pad's half-tilted stick moves at a
    /// quarter), so a controller wanting a share tilts its square root.
    fn tilt_for_share(share: i32) -> i32 {
        psx_math::int32::isqrt_i32(share.clamp(0, 4096) * 4096)
    }

    /// Ask to move along world heading `yaw` at `share` (Q12 share of the
    /// pace, 4096 for full).
    fn request_heading(&mut self, yaw: Angle, share: i32, face: Option<Angle>, run: bool) {
        let strength = Self::tilt_for_share(share);
        let movement = CombatMove::Move {
            x: yaw.sin().mul_i32(strength) as i16,
            z: yaw.cos().mul_i32(strength) as i16,
            forward: 0,
        };
        self.request(movement, face, run);
    }

    /// Spend this tick's motor input on entity `index`: the one motor
    /// update an entity gets per tick. The heading it probed learns whether
    /// the motor moved it.
    #[inline(never)]
    fn drive(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        mover: &mut impl FighterWorld,
        delta: u16,
    ) -> CharacterMotorFrame {
        self.driven = true;
        let input = core::mem::replace(&mut self.motion, NO_MOTION);
        let config = self.motor_config(record);
        // Nothing asked of a body at rest: the update would change nothing,
        // so skip its collision work (most thinking enemies, most ticks).
        if input == NO_MOTION {
            if let Some(frame) = self.motors[index].rest(&config) {
                self.moving[index] = 0;
                self.motor_anims[index] = frame.anim;
                return frame;
            }
        }
        let frame = mover.move_body(
            index,
            record.room,
            &mut self.motors[index],
            input,
            config,
            delta.max(1),
        );
        self.motor_anims[index] = frame.anim;
        self.moving[index] = if !frame.moved {
            0
        } else if input.sprint || !self.motors[index].action().is_idle() {
            2
        } else {
            1
        };
        if let Some(heading) = self.probing.take() {
            self.heading_feedback(index, heading, frame.moved);
        }
        frame
    }

    /// What the body's move along the compass `heading` it tried told the
    /// heading search: keep it while it moves, try the next when blocked.
    pub(super) fn heading_feedback(&mut self, index: usize, heading: u8, moved: bool) {
        if moved {
            let yaw = u16::from(heading) * GAME_ENTITY_DIRECTION_STEP;
            if self.move_yaw[index] == yaw && self.move_yaw_valid[index] > 1 {
                self.move_yaw_valid[index] -= 1;
            } else {
                self.move_yaw[index] = yaw;
                self.move_yaw_valid[index] = Self::reaim_countdown(index);
            }
            self.move_tried[index] = 0;
        } else {
            // Blocked: the search goes on from here next tick.
            self.move_tried[index] |= 1 << heading;
            self.move_yaw_valid[index] = 0;
            if self.move_tried[index] == u8::MAX {
                self.move_tried[index] = 0;
            }
        }
    }

    /// Steer toward `goal` in XZ, covering at most `speed` engine units
    /// this tick: the move a controller asks for. Returns `true` once the
    /// entity stands on the goal (within a unit; the motor carries
    /// sub-unit steps).
    ///
    /// Within one step of the goal the move heads straight onto it.
    /// Further out the entity walks its working heading while that heading
    /// still moves it and does not point away from the goal, and re-aims at
    /// the goal when a per-entity countdown runs out. A heading the motor
    /// could not move along starts a search that ranks the eight headings by
    /// how directly they close on the goal and saves the reverse of the
    /// blocked heading for last; each tick tries one heading, so a long
    /// search resumes on the next tick from `move_tried`.
    // Cold AI-side helper: one small copy on the PS1 instead of one per caller.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn steer(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        goal: [i32; 3],
        speed: i32,
        delta: u16,
        run: bool,
    ) -> (CombatMove, bool) {
        let speed = speed.max(1);
        let dx = goal[0].saturating_sub(self.px(index));
        let dz = goal[2].saturating_sub(self.pz(index));
        if within_xz([dx, dz], [0, 0], 1) {
            self.move_yaw_valid[index] = 0;
            self.move_tried[index] = 0;
            return (CombatMove::Still, true);
        }
        if within_xz([dx, dz], [0, 0], speed) {
            self.move_yaw_valid[index] = 0;
            self.move_tried[index] = 0;
            return (self.move_toward(record, dx, dz, delta, run), false);
        }
        let goal_direction = Self::scaled_direction(dx, dz);
        let current = Self::heading_of(self.move_yaw[index]);
        let keep = self.move_yaw_valid[index] > 1
            && Self::heading_score(current, goal_direction) >= 0
            && self.move_tried[index] & (1 << current) == 0;
        let heading = if keep {
            current
        } else {
            if self.move_yaw_valid[index] == 0 && self.move_tried[index] == 0 {
                // A fresh choice with no heading anchors on the best heading.
                let best = Self::ranked_headings(goal_direction, None, index)[0];
                self.move_yaw[index] = u16::from(best) * GAME_ENTITY_DIRECTION_STEP;
            }
            self.move_yaw_valid[index] = 0;
            let anchor = Self::heading_of(self.move_yaw[index]);
            let tried = self.move_tried[index];
            Self::ranked_headings(goal_direction, Some(anchor), index)
                .into_iter()
                .find(|heading| tried & (1 << heading) == 0)
                .unwrap_or(anchor)
        };
        self.probing = Some(heading);
        let unit = Self::HEADING_UNIT_Q12[usize::from(heading & 7)];
        let dx = Self::q12_step_component(unit[0], speed);
        let dz = Self::q12_step_component(unit[1], speed);
        (self.move_toward(record, dx, dz, delta, run), false)
    }

    /// [`Self::steer`] straight into this tick's motor input, turning the
    /// body into the move. Returns `true` once on the goal.
    // Cold AI-side helper: one small copy on the PS1 instead of one per caller.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn step_toward(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        goal: [i32; 3],
        speed: i32,
        delta: u16,
    ) -> bool {
        let run = speed
            > self
                .walk_units(record)
                .saturating_mul(i32::from(delta.max(1)));
        let (movement, arrived) = self.steer(record, index, goal, speed, delta, run);
        if let CombatMove::Move { x, z, .. } = movement {
            let face = Angle::from_q12(atan2_q12(i32::from(x), i32::from(z)));
            self.request(movement, Some(face), run);
        }
        arrived
    }

    /// Unit vector of each compass heading in Q12, in the motor's yaw
    /// convention (heading `k` is yaw `k * 512`; x = sin, z = cos).
    const HEADING_UNIT_Q12: [[i32; 2]; 8] = [
        [0, 4096],
        [2896, 2896],
        [4096, 0],
        [2896, -2896],
        [0, -4096],
        [-2896, -2896],
        [-4096, 0],
        [-2896, 2896],
    ];

    /// Successful steps a working heading is kept before the entity
    /// re-aims at its goal. The per-entity offset keeps a group of
    /// entities from re-aiming on the same tick.
    fn reaim_countdown(index: usize) -> u8 {
        8 + (index & 7) as u8
    }

    fn heading_of(yaw: u16) -> u8 {
        ((yaw & GAME_ENTITY_YAW_MASK) / GAME_ENTITY_DIRECTION_STEP) as u8
    }

    /// The goal offset shifted down until a Q12 dot product with a heading
    /// unit vector cannot overflow `i32`; only its direction matters.
    fn scaled_direction(dx: i32, dz: i32) -> [i32; 2] {
        let mut direction = [dx, dz];
        while direction[0].unsigned_abs() >= 1 << 18 || direction[1].unsigned_abs() >= 1 << 18 {
            direction = [direction[0] >> 1, direction[1] >> 1];
        }
        direction
    }

    /// How directly `heading` closes on the goal: the goal offset projected
    /// onto the heading's unit vector. Negative means moving away.
    fn heading_score(heading: u8, direction: [i32; 2]) -> i32 {
        let unit = Self::HEADING_UNIT_Q12[usize::from(heading & 7)];
        unit[0] * direction[0] + unit[1] * direction[1]
    }
    /// The eight headings, most goal-closing first: the heading nearest the
    /// goal, then alternately one step either side, starting on the side the
    /// goal leans to (the order of their projections on the goal), and the
    /// heading away from it last. A goal exactly on a heading leans toward
    /// the anchor, else to the entity's preferred turning side. The anchor's
    /// reverse is moved to the end whatever its rank.
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn ranked_headings(direction: [i32; 2], anchor: Option<u8>, index: usize) -> [u8; 8] {
        let yaw = atan2_q12(direction[0], direction[1]) & GAME_ENTITY_YAW_MASK;
        let nearest = (((yaw + 256) >> 9) & 7) as u8;
        // The goal's lean off the nearest heading, -256..=255.
        let lean = (yaw.wrapping_sub(u16::from(nearest) * 512).wrapping_add(256)
            & GAME_ENTITY_YAW_MASK) as i32
            - 256;
        let toward_anchor = anchor.map(|anchor| anchor.wrapping_sub(nearest) & 7);
        let clockwise = match (lean, toward_anchor) {
            (lean, _) if lean != 0 => lean > 0,
            (_, Some(turn)) if turn != 0 && turn != 4 => turn < 4,
            _ => index & 1 == 0,
        };
        let step: u8 = if clockwise { 1 } else { 7 };
        let mut order = [nearest; 8];
        for rank in 1..8u8 {
            let reach = rank.div_ceil(2);
            let heading = if rank % 2 == 1 {
                nearest.wrapping_add(step.wrapping_mul(reach))
            } else {
                nearest.wrapping_sub(step.wrapping_mul(reach))
            };
            order[usize::from(rank)] = heading & 7;
        }
        if let Some(anchor) = anchor {
            let reverse = (anchor + 4) & 7;
            if let Some(at) = order.iter().position(|&heading| heading == reverse) {
                order[at..].rotate_left(1);
            }
        }
        order
    }

    /// Preserve a non-zero component for one-unit low-detail speeds: the
    /// runtime's integer room coordinates would otherwise drop a diagonal
    /// step's sub-unit component and stall the entity.
    fn q12_step_component(direction_q12: i32, speed: i32) -> i32 {
        let product = direction_q12.saturating_mul(speed.max(1));
        let component = product >> 12;
        if component == 0 && product != 0 {
            product.signum()
        } else {
            component
        }
    }

}

/// Clamped integer XZ radius test: early-out on either axis, then an
/// exact squared compare in i32 (radius clamps to 32,767 so the sum
/// of two squares stays inside i32).
fn within_xz(a: [i32; 2], b: [i32; 2], radius: i32) -> bool {
    let radius = radius.clamp(0, i32::from(i16::MAX));
    let dx = a[0].saturating_sub(b[0]);
    let dz = a[1].saturating_sub(b[1]);
    if dx.abs() > radius || dz.abs() > radius {
        return false;
    }
    dx * dx + dz * dz <= radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_level::CombatStats;

    #[test]
    fn a_heavy_armours_by_its_authored_window_else_by_its_contact() {
        static RECORDS: [LevelGameEntityRecord; 1] =
            [test_record(0, 0, 0, 400, game_entity_flags::ENABLED)];
        let mut entities = GameEntities::<4>::EMPTY;
        entities.spawn_from_records(&RECORDS);
        entities.attack_mode[0] = GAME_ENTITY_ATTACK_HEAVY;
        let at = |entities: &mut GameEntities<4>, state, ticks| {
            entities.state[0] = state as u8;
            entities.state_ticks[0] = ticks;
            entities.armoured(&RECORDS, 0)
        };
        // No authored window: armoured only while its contact is live.
        assert!(!at(&mut entities, GameEntityState::Windup, 3));
        assert!(at(&mut entities, GameEntityState::Attack, 0));
        // Its window on the clip clock, ticks 6..12 from the attack's start.
        entities.attack_timings[0][usize::from(GAME_ENTITY_ATTACK_HEAVY)] =
            crate::fighter_action::ActionTiming {
                windup: 10,
                active: 4,
                recovery: 20,
                armour: Some((6, 12)),
                invulnerable: None,
            };
        assert!(!at(&mut entities, GameEntityState::Windup, 5));
        assert!(at(&mut entities, GameEntityState::Windup, 6));
        assert!(at(&mut entities, GameEntityState::Attack, 1));
        assert!(
            !at(&mut entities, GameEntityState::Attack, 2),
            "window closed"
        );
        // A light attack has no armour of its own.
        entities.attack_mode[0] = GAME_ENTITY_ATTACK_LIGHT;
        assert!(!at(&mut entities, GameEntityState::Attack, 0));
    }

    #[test]
    fn an_entity_attack_reads_its_characters_authored_invulnerable_window() {
        static RECORDS: [LevelGameEntityRecord; 1] =
            [test_record(0, 0, 0, 400, game_entity_flags::ENABLED)];
        let mut entities = GameEntities::<4>::EMPTY;
        entities.spawn_from_records(&RECORDS);
        entities.attack_mode[0] = GAME_ENTITY_ATTACK_LIGHT;
        let at = |entities: &mut GameEntities<4>, state, ticks| {
            entities.state[0] = state as u8;
            entities.state_ticks[0] = ticks;
            entities.invulnerable(&RECORDS, 0)
        };
        // No authored window: nothing protects the attack.
        assert!(!at(&mut entities, GameEntityState::Windup, 6));
        // Ticks 6..12 from the attack's start, as the player's own window.
        entities.attack_timings[0][usize::from(GAME_ENTITY_ATTACK_LIGHT)] =
            crate::fighter_action::ActionTiming {
                windup: 10,
                active: 4,
                recovery: 20,
                armour: None,
                invulnerable: Some((6, 12)),
            };
        assert!(!at(&mut entities, GameEntityState::Windup, 5));
        assert!(at(&mut entities, GameEntityState::Windup, 6));
        assert!(at(&mut entities, GameEntityState::Attack, 1));
        assert!(!at(&mut entities, GameEntityState::Attack, 2));
    }

    #[test]
    fn an_attack_runs_on_the_timing_read_off_its_clip_else_on_its_record() {
        static RECORDS: [LevelGameEntityRecord; 2] = [
            test_record(0, 0, 0, 400, game_entity_flags::ENABLED),
            test_record(64, 0, 0, 400, game_entity_flags::ENABLED),
        ];
        let mut entities = GameEntities::<4>::EMPTY;
        entities.spawn_from_records(&RECORDS);
        let clip = crate::fighter_action::ActionTiming {
            windup: 9,
            active: 4,
            recovery: 21,
            armour: None,
            invulnerable: None,
        };
        // Only the first entity's light attack has a clip with contact.
        entities.time_attacks(&RECORDS, |record, action, _, _, _| {
            (record.x == 0 && action == CharacterAnimationAction::LightAttack).then_some(clip)
        });
        let mut stats = GameEntityTickStats::default();
        for index in 0..2 {
            entities.enter_state(index, GameEntityState::Windup, &mut stats);
        }
        assert_eq!(
            entities.committed_swing(&RECORDS, 0),
            Some((CharacterAnimationAction::LightAttack, 9, 12))
        );
        let windup = u16::from(RECORDS[1].windup_ticks);
        let active = RECORDS[1]
            .attack_active_ticks
            .max(GAME_ENTITY_ATTACK_ACTIVE_TICKS);
        assert_eq!(
            entities.committed_swing(&RECORDS, 1),
            Some((
                CharacterAnimationAction::LightAttack,
                windup,
                windup + active - 1
            ))
        );
        // A spawn goes back to the record's ticks until the game times it.
        entities.spawn_from_records(&RECORDS);
        entities.enter_state(0, GameEntityState::Windup, &mut stats);
        assert_eq!(
            entities.committed_swing(&RECORDS, 0).map(|swing| swing.1),
            Some(windup)
        );
    }

    /// The test records' swap i-frames: the editor's default
    /// `roll_invulnerable_frames`.
    pub(super) const TEST_SWAP_INVULNERABLE_TICKS: u8 = 10;

    /// Motion numbers of a test character walking `walk` and running `run`
    /// whole units per frame, with the editor's default roll.
    pub(super) const fn test_motion(walk: i32, run: i32) -> psx_level::CharacterMotion {
        psx_level::CharacterMotion {
            walk_speed: walk * 256,
            run_speed: run * 256,
            turn_speed_degrees_per_second: 180,
            weight_q8: 256,
            stamina_max_q12: 4096,
            sprint_min_q12: 384,
            sprint_drain_q12: 40,
            stamina_recover_q12: 36,
            roll_cost_q12: 768,
            roll_speed: 6 << 8,
            roll_active_frames: 14,
            roll_recovery_frames: 12,
            roll_invulnerable_frames: TEST_SWAP_INVULNERABLE_TICKS,
        }
    }

    /// A test world written as the old per-step rule: the real motor moves
    /// the body through open space, then the rule decides where that step
    /// may end. A host-test shape only.
    pub(crate) trait StepRule {
        #[allow(clippy::too_many_arguments)]
        fn step(
            &mut self,
            entity: usize,
            room: RoomIndex,
            position: [i32; 3],
            dx: i32,
            dz: i32,
            radius: i32,
            height: i32,
        ) -> [i32; 3];

        fn line_of_sight(&mut self, _room: RoomIndex, _from: [i32; 3], _to: [i32; 3]) -> bool {
            true
        }
    }

    impl<T: StepRule> FighterWorld for T {
        fn move_body(
            &mut self,
            entity: usize,
            room: RoomIndex,
            motor: &mut CharacterMotorState,
            input: CharacterMotorInput,
            config: CharacterMotorConfig,
            delta: u16,
        ) -> CharacterMotorFrame {
            let start = motor.position();
            let mut frame = OpenWorld.move_body(entity, room, motor, input, config, delta);
            let end = frame.position;
            if end.x != start.x || end.z != start.z {
                let allowed = self.step(
                    entity,
                    room,
                    [start.x, start.y, start.z],
                    end.x - start.x,
                    end.z - start.z,
                    config.radius,
                    config.height,
                );
                if allowed != [end.x, end.y, end.z] {
                    let p = RoomPoint::new(allowed[0], allowed[1], allowed[2]);
                    motor.relocate(p);
                    frame.position = p;
                    frame.moved = p.x != start.x || p.z != start.z;
                    frame.blocked = true;
                    if !frame.moved {
                        // The motor's own rule: a refused move stands still.
                        frame.anim = CharacterMotorAnim::Idle;
                    }
                }
            }
            frame
        }

        fn push_body(
            &mut self,
            entity: usize,
            room: RoomIndex,
            position: [i32; 3],
            dx: i32,
            dz: i32,
            radius: i32,
            height: i32,
        ) -> [i32; 3] {
            self.step(entity, room, position, dx, dz, radius, height)
        }

        fn line_of_sight(&mut self, room: RoomIndex, from: [i32; 3], to: [i32; 3]) -> bool {
            StepRule::line_of_sight(self, room, from, to)
        }
    }

    pub(super) const fn test_record(
        x: i32,
        z: i32,
        patrol_dx: i32,
        aggro_radius: u16,
        flags: u16,
    ) -> LevelGameEntityRecord {
        LevelGameEntityRecord {
            room: RoomIndex(0),
            kind: 1,
            targetname: 0,
            model_instance: psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE,
            character: psx_level::CharacterIndex(BODY_STANDARD),
            idle_clip: 0,
            alert_clip: 9,
            turn_clip: 10,
            walk_clip: 1,
            walk_backward_clip: 6,
            strafe_left_clip: 7,
            strafe_right_clip: 8,
            run_clip: 2,
            attack_clip: 3,
            attack_speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            attack_frame_range: CharacterActionFrameRange::FULL,
            heavy_attack_clip: 11,
            heavy_attack_speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            heavy_attack_frame_range: CharacterActionFrameRange::FULL,
            ranged_attack_clip: 12,
            ranged_attack_speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            ranged_attack_frame_range: CharacterActionFrameRange::FULL,
            stagger_clip: 4,
            stagger_speed_q8: 0,
            stagger_frame_range: CharacterActionFrameRange::FULL,
            stagger_ticks: 0,
            knockdown_clip: 4,
            parried_clip: 4,
            roll_clip: 4,
            death_clip: 5,
            ranged_attack_action: CharacterAnimationAction::RangedAttack.to_index() as u8,
            x,
            y: 0,
            z,
            yaw: 0,
            patrol_x: x + patrol_dx,
            patrol_y: 0,
            patrol_z: z,
            patrol_wait_ticks: 2,
            aggro_radius,
            reaction_ticks: 0,
            preferred_distance: 512,
            spacing_tolerance: 0,
            spacing_speed_percent: 100,
            decision_interval_ticks: 1,
            circle_chance: 0,
            attack_priority: 1,
            attack_cooldown_ticks: 0,
            group_attack_delay_ticks: 0,
            windup_ticks: 3,
            attack_active_ticks: GAME_ENTITY_ATTACK_ACTIVE_TICKS,
            heavy_attack_active_ticks: GAME_ENTITY_ATTACK_ACTIVE_TICKS + 1,
            ranged_attack_active_ticks: GAME_ENTITY_ATTACK_ACTIVE_TICKS + 2,
            recovery_ticks: 4,
            attack_min_range: 0,
            attack_max_range: 0,
            touch_damage: 10,
            soul_value: 0,
            flags,
        }
    }

    /// The test entities' combat numbers: one 100 bar, poise 50, the old
    /// enemy parry window of 3.
    pub(super) const TEST_COMBAT: CombatStats = CombatStats {
        poise: 50,
        max_health: 100,
        parry_ticks: 3,
        ..CombatStats::DEFAULT
    };

    /// A test body: capsule, motion and combat numbers, the old fixed
    /// 12-tick swap and no swap cooldown (what every test ran with before
    /// bodies were Characters).
    pub(super) const fn test_body(
        radius: u16,
        height: u16,
        motion: psx_level::CharacterMotion,
        combat: CombatStats,
    ) -> LevelCharacterRecord {
        LevelCharacterRecord {
            radius,
            height,
            walk_speed: motion.walk_speed,
            run_speed: motion.run_speed,
            turn_speed_degrees_per_second: motion.turn_speed_degrees_per_second,
            weight_q8: motion.weight_q8,
            stamina_max_q12: motion.stamina_max_q12,
            sprint_min_q12: motion.sprint_min_q12,
            sprint_drain_q12: motion.sprint_drain_q12,
            stamina_recover_q12: motion.stamina_recover_q12,
            roll_cost_q12: motion.roll_cost_q12,
            roll_speed: motion.roll_speed,
            roll_active_frames: motion.roll_active_frames,
            roll_recovery_frames: motion.roll_recovery_frames,
            roll_invulnerable_frames: motion.roll_invulnerable_frames,
            combat,
            stance_swap_duration_ticks: 12,
            stance_swap_cooldown_ticks: 0,
            ..LevelCharacterRecord::DEFAULT_BODY
        }
    }

    /// The bodies the test records link (by these indices).
    pub(super) const BODY_STANDARD: u16 = 0;
    pub(super) const BODY_TACTICAL: u16 = 1;
    // Index 2 was the hybrid body of the retired distance-only controller tests.
    pub(super) const BODY_SMALL: u16 = 3;
    pub(super) const BODY_NO_KNOCKDOWN: u16 = 4;
    pub(super) const BODY_SHORT_SWAP: u16 = 5;
    pub(super) const BODY_SUB_UNIT: u16 = 6;
    const TACTICAL_MOTION: psx_level::CharacterMotion = psx_level::CharacterMotion {
        walk_speed: 2 * 256,
        run_speed: 6 * 256,
        ..test_motion(16, 48)
    };
    pub(super) static TEST_BODIES: [LevelCharacterRecord; 7] = [
        test_body(192, 1024, test_motion(16, 48), TEST_COMBAT),
        // The two old 600 pools as one bar.
        test_body(
            14,
            77,
            TACTICAL_MOTION,
            CombatStats {
                max_health: 1200,
                ..TEST_COMBAT
            },
        ),
        test_body(14, 77, test_motion(1, 6), TEST_COMBAT),
        test_body(12, 64, test_motion(16, 48), TEST_COMBAT),
        // Knockdown off: a break then staggers.
        test_body(
            192,
            1024,
            test_motion(16, 48),
            CombatStats {
                knockdown_ticks: 0,
                ..TEST_COMBAT
            },
        ),
        test_body(
            192,
            1024,
            psx_level::CharacterMotion {
                roll_invulnerable_frames: 2,
                ..test_motion(16, 48)
            },
            TEST_COMBAT,
        ),
        // The cooked training enemy's walk (512 Q8) at the 3/4 speed knob.
        test_body(
            14,
            77,
            psx_level::CharacterMotion {
                walk_speed: 384,
                ..TACTICAL_MOTION
            },
            CombatStats {
                max_health: 1200,
                ..TEST_COMBAT
            },
        ),
    ];

    static IDLE_ENEMY: [LevelGameEntityRecord; 1] = [test_record(
        1000,
        1000,
        0,
        512,
        game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
    )];
    static PATROL_ENEMY: [LevelGameEntityRecord; 1] = [test_record(
        1000,
        1000,
        400,
        512,
        game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
    )];
    static TARGETED_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        model_instance: 7,
        ..test_record(
            1000,
            1000,
            0,
            512,
            game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
        )
    }];
    static DISABLED_ENEMY: [LevelGameEntityRecord; 1] = [test_record(0, 0, 0, 512, 0)];
    static RANGED_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        aggro_radius: 4096,
        preferred_distance: 1200,
        spacing_tolerance: 100,
        spacing_speed_percent: 100,
        attack_min_range: 500,
        attack_max_range: 1600,
        flags: game_entity_flags::ENABLED
            | game_entity_flags::CAN_RUN
            | game_entity_flags::RANGED_ATTACK
            | game_entity_flags::MELEE_ATTACK,
        ..test_record(
            1000,
            1000,
            0,
            4096,
            game_entity_flags::ENABLED
                | game_entity_flags::CAN_RUN
                | game_entity_flags::RANGED_ATTACK
                | game_entity_flags::MELEE_ATTACK,
        )
    }];

    fn far_input() -> GameEntityTickInput<'static> {
        GameEntityTickInput {
            target: CombatTarget {
                position: [100_000, 0, 100_000],
                radius: 192,
                height: 1024,
                noise_radius: 0,
                defender: Defender::OPEN,
                stance: None,
                attack_read: 0,
                swing_ticks: None,
            },
        }
    }

    fn near_input() -> GameEntityTickInput<'static> {
        GameEntityTickInput {
            target: CombatTarget {
                position: [1200, 0, 1000],
                radius: 192,
                height: 1024,
                noise_radius: 0,
                defender: Defender::OPEN,
                stance: None,
                attack_read: 0,
                swing_ticks: None,
            },
        }
    }

    /// Mover that refuses every step (a body wedged into a corner).
    struct BlockedMover;
    impl StepRule for BlockedMover {
        fn step(
            &mut self,
            _entity: usize,
            _room: RoomIndex,
            position: [i32; 3],
            _dx: i32,
            _dz: i32,
            _radius: i32,
            _height: i32,
        ) -> [i32; 3] {
            position
        }
    }

    #[derive(Default)]
    struct SightMover {
        clear: bool,
        queries: u16,
        last_from: [i32; 3],
        last_to: [i32; 3],
    }

    impl StepRule for SightMover {
        fn step(
            &mut self,
            _entity: usize,
            _room: RoomIndex,
            position: [i32; 3],
            _dx: i32,
            _dz: i32,
            _radius: i32,
            _height: i32,
        ) -> [i32; 3] {
            position
        }

        fn line_of_sight(&mut self, _room: RoomIndex, from: [i32; 3], to: [i32; 3]) -> bool {
            self.queries = self.queries.saturating_add(1);
            self.last_from = from;
            self.last_to = to;
            self.clear
        }
    }

    /// Test collision with one finite wall. It rejects a candidate whose
    /// endpoint enters the wall and otherwise commits it, matching the
    /// accept/hold contract the real mover exposes.
    #[derive(Default)]
    struct FiniteWallMover {
        calls: usize,
    }

    impl StepRule for FiniteWallMover {
        fn step(
            &mut self,
            _entity: usize,
            _room: RoomIndex,
            position: [i32; 3],
            dx: i32,
            dz: i32,
            _radius: i32,
            _height: i32,
        ) -> [i32; 3] {
            self.calls += 1;
            let target = [
                position[0].saturating_add(dx),
                position[1],
                position[2].saturating_add(dz),
            ];
            let inside_wall =
                (1080..=1240).contains(&target[0]) && (900..=1100).contains(&target[2]);
            if inside_wall {
                position
            } else {
                target
            }
        }
    }

    #[derive(Default)]
    struct CountingBlockedMover {
        calls: usize,
    }

    impl StepRule for CountingBlockedMover {
        fn step(
            &mut self,
            _entity: usize,
            _room: RoomIndex,
            position: [i32; 3],
            _dx: i32,
            _dz: i32,
            _radius: i32,
            _height: i32,
        ) -> [i32; 3] {
            self.calls += 1;
            position
        }
    }

    #[test]
    fn prototype_both_colours_damage_and_share_poise() {
        let mut e = GameEntities::<8>::EMPTY;
        e.spawn_from_records(&DUAL_ENEMY);
        assert_eq!(
            e.scaled_stance_damage(&DUAL_ENEMY, 0, VitalityChannelId::One, 40),
            40
        );
        assert_eq!(
            e.scaled_stance_damage(&DUAL_ENEMY, 0, VitalityChannelId::Two, 40),
            50
        );
        let capacity = TEST_COMBAT.poise;
        assert!(
            !e.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 1, capacity / 2)
                .staggered
        );
        assert!(
            e.apply_hit(
                &DUAL_ENEMY,
                0,
                VitalityChannelId::Two,
                1,
                capacity - capacity / 2
            )
            .staggered
        );
        let ticks = e.state_ticks[0];
        assert!(
            !e.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 1, capacity * 2)
                .staggered
        );
        assert_eq!(e.state_ticks[0], ticks);
    }
    #[test]
    fn prototype_shot_interrupts_late_tell_and_cancels_retained_attack() {
        let mut e = GameEntities::<8>::EMPTY;
        e.spawn_from_records(&DUAL_ENEMY);
        e.enter_state(
            0,
            GameEntityState::Windup,
            &mut GameEntityTickStats::default(),
        );
        e.state_ticks[0] = 0;
        assert!(!e.shot_opening(&DUAL_ENEMY, 0));
        e.state_ticks[0] = u16::from(DUAL_ENEMY[0].windup_ticks) / 2;
        assert!(e.shot_opening(&DUAL_ENEMY, 0));
        assert!(
            e.apply_projectile_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 1, 10, false)
                .staggered
        );
        assert!(
            !e.apply_projectile_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 1, 10, false)
                .staggered
        );
        for _ in 0..5 {
            e.spend_shot_energy(&DUAL_ENEMY[0], 0);
        }
        assert!(!e.can_fire_energy(&DUAL_ENEMY[0], 0));
        e.gain_melee_energy(&DUAL_ENEMY[0], 0, true);
        assert!(e.can_fire_energy(&DUAL_ENEMY[0], 0));
    }

    #[test]
    fn empty_records_tick_is_inert() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&[]);
        entities.set_spatial_active_mask(u64::MAX);
        let stats = entities.tick(&[], far_input(), &mut OpenWorld);
        assert_eq!(entities.count(), 0);
        assert_eq!(stats, GameEntityTickStats::default());
    }

    #[test]
    fn an_idle_entity_a_hit_reaches_turns_on_its_attacker() {
        static ENEMY: [LevelGameEntityRecord; 1] = [test_record(
            1000,
            1000,
            0,
            512,
            game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
        )];
        // One record and the actor slot: the player shoots from behind.
        let mut entities = GameEntities::<2>::EMPTY;
        entities.spawn_from_records(&ENEMY);
        assert_eq!(entities.state(0), GameEntityState::Idle);
        let out = entities.apply_projectile_hit(&ENEMY, 0, VitalityChannelId::One, 1, 0, false);
        assert!(out.connected);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // The same for a hit from another entity.
        let mut pair = GameEntities::<2>::EMPTY;
        pair.spawn_from_records(&ENEMY);
        pair.receive_hit(
            &ENEMY,
            0,
            Strike {
                attacker: CombatantId::entity(1),
                ..player_strike(HitKind::Melee, VitalityChannelId::One, 1, 0, false)
            },
        );
        assert_eq!(pair.state(0), GameEntityState::Aggro);
    }

    #[test]
    fn spawn_copies_records_and_disabled_spawn_dead() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        assert_eq!(entities.count(), 1);
        assert_eq!(entities.state(0), GameEntityState::Idle);
        assert_eq!(entities.position(0), [1000, 0, 1000]);
        assert_eq!(entities.health(0), 100);

        entities.spawn_from_records(&DISABLED_ENEMY);

        entities.set_spatial_active_mask(u64::MAX);
        assert_eq!(entities.state(0), GameEntityState::Dead);
    }

    #[test]
    fn committed_attacks_do_not_swap_even_with_an_expired_cooldown() {
        for state in [
            GameEntityState::Windup,
            GameEntityState::Attack,
            GameEntityState::Recover,
        ] {
            let mut e = GameEntities::<8>::EMPTY;
            e.spawn_from_records(&RANGED_ENEMY);
            e.select_attack(0, false);
            e.enter_state(0, state, &mut GameEntityTickStats::default());
            let input = GameEntityTickInput {
                target: CombatTarget {
                    position: [2550, 0, 1000],
                    ..near_input().target
                },
            };
            e.tick_delta(&RANGED_ENEMY, input, &mut BlockedMover, 1);
            assert_eq!(e.stance(0), VitalityChannelId::One, "swap during {state:?}");
        }
    }

    #[test]
    fn close_attacks_alternate_light_heavy_without_ranged_consuming_the_sequence() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&RANGED_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);

        entities.select_attack(0, false);
        assert_eq!(
            entities.selected_attack_action(&RANGED_ENEMY[0], 0),
            CharacterAnimationAction::LightAttack
        );
        assert_eq!(entities.selected_attack_clip(&RANGED_ENEMY[0], 0), 3);

        entities.select_attack(0, true);
        assert_eq!(
            entities.selected_attack_action(&RANGED_ENEMY[0], 0),
            CharacterAnimationAction::RangedAttack
        );
        assert_eq!(entities.selected_attack_clip(&RANGED_ENEMY[0], 0), 12);

        entities.select_attack(0, false);
        assert_eq!(
            entities.selected_attack_action(&RANGED_ENEMY[0], 0),
            CharacterAnimationAction::HeavyAttack
        );
        assert_eq!(entities.selected_attack_clip(&RANGED_ENEMY[0], 0), 11);

        entities.select_attack(0, false);
        assert_eq!(
            entities.selected_attack_action(&RANGED_ENEMY[0], 0),
            CharacterAnimationAction::LightAttack
        );
    }

    #[test]
    fn model_instance_lookup_tracks_live_position_and_rejects_dead_entities() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&TARGETED_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        assert_eq!(
            entities.live_position_for_model_instance(&TARGETED_ENEMY, 7),
            Some([1000, 0, 1000])
        );

        entities.set_axis(0, 0, 1234);
        entities.set_axis(0, 2, 876);
        assert_eq!(
            entities.live_position_for_model_instance(&TARGETED_ENEMY, 7),
            Some([1234, 0, 876])
        );
        assert_eq!(
            entities.live_position_for_model_instance(&TARGETED_ENEMY, 8),
            None
        );

        entities.state[0] = GameEntityState::Dead as u8;
        assert_eq!(
            entities.live_position_for_model_instance(&TARGETED_ENEMY, 7),
            None
        );
    }

    #[test]
    fn spawn_clamps_to_capacity_and_counts_overflow() {
        static MANY: [LevelGameEntityRecord; 3] = [
            test_record(
                0,
                0,
                0,
                512,
                game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
            ),
            test_record(
                100,
                0,
                0,
                512,
                game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
            ),
            test_record(
                200,
                0,
                0,
                512,
                game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
            ),
        ];
        let mut entities = GameEntities::<2>::EMPTY;
        entities.spawn_from_records(&MANY);
        entities.set_spatial_active_mask(u64::MAX);
        assert_eq!(entities.count(), 2);
        assert_eq!(entities.overflow_count(), 1);
    }

    #[test]
    fn acquisition_reaction_plays_the_alert_one_shot() {
        static REACTIVE: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            reaction_ticks: 3,
            ..test_record(
                1000,
                1000,
                0,
                512,
                game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
            )
        }];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&REACTIVE);
        entities.set_spatial_active_mask(u64::MAX);

        entities.tick(&REACTIVE, near_input(), &mut OpenWorld);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        assert_eq!(
            entities.clip_for_state(&REACTIVE, 0),
            GameEntityClip {
                clip: 9,
                phase_ticks: 0,
                one_shot: true,
                ..GameEntityClip::default()
            }
        );

        entities.tick(&REACTIVE, near_input(), &mut OpenWorld);
        assert_eq!(entities.clip_for_state(&REACTIVE, 0).phase_ticks, 1);
        entities.tick(&REACTIVE, near_input(), &mut OpenWorld);
        entities.tick(&REACTIVE, near_input(), &mut OpenWorld);
        assert_ne!(entities.clip_for_state(&REACTIVE, 0).clip, 9);
    }

    #[test]
    fn aggro_deaggros_past_leash_and_patrol_walks_legs() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        // Aggro from proximity...
        entities.tick(&PATROL_ENEMY, near_input(), &mut OpenWorld);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // ...then the player leaves: leash drop back to Idle.
        entities.tick(&PATROL_ENEMY, far_input(), &mut OpenWorld);
        assert_eq!(entities.state(0), GameEntityState::Idle);
        // Idle waits patrol_wait_ticks (2) then patrols to the anchor
        // 400 units away at the record's Character walk speed.
        entities.tick(&PATROL_ENEMY, far_input(), &mut OpenWorld);
        let stats = entities.tick(&PATROL_ENEMY, far_input(), &mut OpenWorld);
        assert_eq!(entities.state(0), GameEntityState::Patrol);
        assert_eq!(stats.patrol_enters, 1);
        let mut walked = 0;
        while entities.state(0) == GameEntityState::Patrol && walked < 100 {
            entities.tick(&PATROL_ENEMY, far_input(), &mut OpenWorld);
            walked += 1;
        }
        assert_eq!(entities.state(0), GameEntityState::Idle);
        assert!((entities.position(0)[0] - 1400).abs() <= 1);
        // Walking the +X leg turns the body toward +X (a quarter turn, 1024
        // PSX units) at the character's own turn rate, as the player turns.
        let yaw = entities.yaw(0);
        assert!(yaw > 512 && yaw <= 1024, "yaw {yaw}");
    }

    #[test]
    fn blocked_mover_holds_position_but_state_machine_still_runs() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        entities.tick(&PATROL_ENEMY, far_input(), &mut BlockedMover);
        entities.tick(&PATROL_ENEMY, far_input(), &mut BlockedMover);
        assert_eq!(entities.state(0), GameEntityState::Patrol);
        for _ in 0..10 {
            entities.tick(&PATROL_ENEMY, far_input(), &mut BlockedMover);
        }
        // Fully blocked: never arrives, never leaves Patrol, position
        // pinned to spawn -- and no state corruption.
        assert_eq!(entities.state(0), GameEntityState::Patrol);
        assert_eq!(entities.position(0), [1000, 0, 1000]);
    }

    #[test]
    fn heading_search_routes_patrol_around_a_finite_wall() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        entities.state[0] = GameEntityState::Patrol as u8;
        let mut mover = FiniteWallMover::default();
        let mut left_direct_line = false;

        for _ in 0..160 {
            entities.tick(&PATROL_ENEMY, far_input(), &mut mover);
            left_direct_line |= entities.position(0)[2] != PATROL_ENEMY[0].z;
            if entities.state(0) == GameEntityState::Idle {
                break;
            }
        }

        assert!(
            left_direct_line,
            "the blocked entity searches around the wall"
        );
        assert_eq!(entities.state(0), GameEntityState::Idle);
        // On the anchor within the unit the motor carries between ticks.
        let p = entities.position(0);
        assert!((p[0] - PATROL_ENEMY[0].patrol_x).abs() <= 1, "{p:?}");
        assert!((p[2] - PATROL_ENEMY[0].patrol_z).abs() <= 1, "{p:?}");
    }

    #[test]
    fn heading_search_is_deterministic_across_identical_runs() {
        let mut first = GameEntities::<8>::EMPTY;
        let mut second = GameEntities::<8>::EMPTY;
        first.spawn_from_records(&PATROL_ENEMY);
        second.spawn_from_records(&PATROL_ENEMY);
        first.state[0] = GameEntityState::Patrol as u8;
        second.state[0] = GameEntityState::Patrol as u8;
        let mut first_mover = FiniteWallMover::default();
        let mut second_mover = FiniteWallMover::default();

        for _ in 0..160 {
            first.tick(&PATROL_ENEMY, far_input(), &mut first_mover);
            second.tick(&PATROL_ENEMY, far_input(), &mut second_mover);
            assert_eq!(first.position(0), second.position(0));
            assert_eq!(first.yaw(0), second.yaw(0));
            assert_eq!(first.state(0), second.state(0));
            assert_eq!(first.move_yaw[0], second.move_yaw[0]);
            assert_eq!(first.move_yaw_valid[0], second.move_yaw_valid[0]);
            assert_eq!(first.move_tried[0], second.move_tried[0]);
        }
        assert_eq!(first_mover.calls, second_mover.calls);
    }

    #[test]
    fn heading_search_probes_each_direction_at_most_once() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        entities.state[0] = GameEntityState::Patrol as u8;
        let mut mover = CountingBlockedMover::default();

        for _ in 0..8 {
            let before = mover.calls;
            entities.tick(&PATROL_ENEMY, far_input(), &mut mover);
            assert!(
                mover.calls - before <= 1,
                "the blocked search tries one heading per tick (one motor update)"
            );
        }

        assert_eq!(mover.calls, 8, "every eight-way direction is probed once");
        assert_eq!(entities.position(0), [1000, 0, 1000]);
    }

    /// Records every probed step delta and refuses all of them.
    #[derive(Default)]
    struct RecordingBlockedMover {
        deltas: [[i32; 2]; 16],
        len: usize,
    }
    impl StepRule for RecordingBlockedMover {
        fn step(
            &mut self,
            _entity: usize,
            _room: RoomIndex,
            position: [i32; 3],
            dx: i32,
            dz: i32,
            _radius: i32,
            _height: i32,
        ) -> [i32; 3] {
            if self.len < self.deltas.len() {
                self.deltas[self.len] = [dx.signum(), dz.signum()];
            }
            self.len += 1;
            position
        }
    }

    #[test]
    fn heading_ranking_closes_on_the_goal_first() {
        type Entities = GameEntities<8>;
        // Goal due east (+x), no anchor: east, then the two diagonals that
        // still close on it, then north/south, then away.
        let order = Entities::ranked_headings([1000, 0], None, 0);
        assert_eq!(order[0], 2);
        assert_eq!(
            {
                let mut pair = [order[1], order[2]];
                pair.sort_unstable();
                pair
            },
            [1, 3]
        );
        assert_eq!(order[7], 6);
        // An anchor moves its own reverse to the very end.
        let anchored = Entities::ranked_headings([1000, 0], Some(0), 0);
        assert_eq!(anchored[7], 4);
        // Ties prefer the heading nearer the anchor.
        let toward_north = Entities::ranked_headings([1000, 0], Some(0), 0);
        assert_eq!(toward_north[1], 1);
    }

    #[test]
    fn blocked_search_tries_the_reverse_heading_last() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        entities.state[0] = GameEntityState::Patrol as u8;
        // A working heading due north that the mover now refuses.
        entities.move_yaw[0] = 0;
        entities.move_yaw_valid[0] = 5;
        let mut mover = RecordingBlockedMover::default();
        for _ in 0..8 {
            entities.tick(&PATROL_ENEMY, far_input(), &mut mover);
        }
        assert_eq!(mover.len, 8);
        assert_eq!(mover.deltas[0], [0, 1], "the working heading goes first");
        assert_eq!(mover.deltas[1], [1, 0], "then the most goal-closing one");
        assert_eq!(mover.deltas[7], [0, -1], "the reverse goes last");
        let mut probed = [[0i32; 2]; 8];
        probed.copy_from_slice(&mover.deltas[..8]);
        probed.sort_unstable();
        assert!(
            probed.windows(2).all(|pair| pair[0] != pair[1]),
            "each heading is probed once"
        );
    }

    #[test]
    fn an_idle_enemy_ignores_a_player_outside_its_notice_range() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        entities.tick(&IDLE_ENEMY, far_input(), &mut OpenWorld);
        assert_eq!(entities.state(0), GameEntityState::Idle);
    }

    #[test]
    fn perception_distinguishes_front_sight_rear_hearing_and_silence() {
        static ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            x: 0,
            y: 0,
            z: 0,
            patrol_x: 0,
            patrol_y: 0,
            patrol_z: 0,
            character: psx_level::CharacterIndex(BODY_SMALL),
            aggro_radius: 352,
            ..IDLE_ENEMY[0]
        }];
        let base = GameEntityTickInput {
            target: CombatTarget {
                position: [0, 0, -180],
                height: 64,
                radius: 12,
                ..near_input().target
            },
        };
        for delta in [1, 2] {
            let mut entities = GameEntities::<8>::EMPTY;
            entities.spawn_from_records(&ENEMY);
            entities.set_spatial_active_mask(0);
            entities.tick_delta(&ENEMY, base, &mut OpenWorld, delta);
            assert_eq!(
                entities.state(0),
                GameEntityState::Idle,
                "silent rear approach"
            );
            let walking = GameEntityTickInput {
                target: CombatTarget {
                    noise_radius: 128,
                    ..base.target
                },
            };
            entities.tick_delta(&ENEMY, walking, &mut OpenWorld, delta);
            assert_eq!(
                entities.state(0),
                GameEntityState::Idle,
                "outside walking earshot"
            );
            let running = GameEntityTickInput {
                target: CombatTarget {
                    noise_radius: 256,
                    ..base.target
                },
            };
            entities.tick_delta(&ENEMY, running, &mut OpenWorld, delta);
            assert_eq!(
                entities.state(0),
                GameEntityState::Aggro,
                "hears running behind"
            );
            entities.spawn_from_records(&ENEMY);
            entities.set_spatial_active_mask(u64::MAX);
            let front = GameEntityTickInput {
                target: CombatTarget {
                    position: [0, 0, 300],
                    ..base.target
                },
            };
            entities.tick_delta(&ENEMY, front, &mut OpenWorld, delta);
            assert_eq!(
                entities.state(0),
                GameEntityState::Aggro,
                "sees ahead without noise"
            );
            entities.spawn_from_records(&ENEMY);
            entities.set_spatial_active_mask(u64::MAX);
            let close = GameEntityTickInput {
                target: CombatTarget {
                    position: [0, 0, -90],
                    ..walking.target
                },
            };
            entities.tick_delta(&ENEMY, close, &mut OpenWorld, delta);
            assert_eq!(
                entities.state(0),
                GameEntityState::Aggro,
                "hears walking behind"
            );
        }
    }

    #[test]
    fn cover_attenuates_hearing_and_still_blocks_attack_commitment() {
        static ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            x: 0,
            y: 0,
            z: 0,
            patrol_x: 0,
            patrol_y: 0,
            patrol_z: 0,
            character: psx_level::CharacterIndex(BODY_SMALL),
            aggro_radius: 352,
            ..IDLE_ENEMY[0]
        }];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        let mut blocked = SightMover::default();
        let input = GameEntityTickInput {
            target: CombatTarget {
                position: [0, 0, -120],
                height: 64,
                radius: 12,
                noise_radius: 256,
                ..near_input().target
            },
        };
        entities.tick(&ENEMY, input, &mut blocked);
        assert_eq!(entities.state(0), GameEntityState::Idle);
        let close = GameEntityTickInput {
            target: CombatTarget {
                position: [0, 0, -40],
                ..input.target
            },
        };
        entities.tick(&ENEMY, close, &mut blocked);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        for _ in 0..10 {
            entities.tick(&ENEMY, close, &mut blocked);
        }
        assert_eq!(
            entities.state(0),
            GameEntityState::Aggro,
            "cannot attack through cover"
        );
        entities.spawn_from_records(&ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        let upstairs = GameEntityTickInput {
            target: CombatTarget {
                position: [0, 512, -40],
                ..input.target
            },
        };
        entities.tick(&ENEMY, upstairs, &mut blocked);
        assert_eq!(entities.state(0), GameEntityState::Idle);
    }

    #[test]
    fn selected_attack_clip_carries_authored_speed_and_trim_range() {
        const LIGHT_RANGE: CharacterActionFrameRange =
            CharacterActionFrameRange { start: 2, end: 53 };
        const HEAVY_RANGE: CharacterActionFrameRange = CharacterActionFrameRange {
            start: 89,
            end: 126,
        };
        static PACED_ATTACK: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            attack_speed_q8: 320,
            attack_frame_range: LIGHT_RANGE,
            heavy_attack_speed_q8: 448,
            heavy_attack_frame_range: HEAVY_RANGE,
            ranged_attack_speed_q8: 640,
            ranged_attack_frame_range: CharacterActionFrameRange::FULL,
            ..test_record(
                1000,
                1000,
                0,
                512,
                game_entity_flags::ENABLED
                    | game_entity_flags::RANGED_ATTACK
                    | game_entity_flags::MELEE_ATTACK,
            )
        }];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PACED_ATTACK);
        entities.set_spatial_active_mask(u64::MAX);
        entities.state[0] = GameEntityState::Windup as u8;

        entities.attack_mode[0] = GAME_ENTITY_ATTACK_LIGHT;
        let light = entities.clip_for_state(&PACED_ATTACK, 0);
        assert_eq!(
            (light.clip, light.speed_q8, light.frame_range),
            (3, 320, LIGHT_RANGE)
        );

        entities.attack_mode[0] = GAME_ENTITY_ATTACK_HEAVY;
        let heavy = entities.clip_for_state(&PACED_ATTACK, 0);
        assert_eq!(
            (heavy.clip, heavy.speed_q8, heavy.frame_range),
            (11, 448, HEAVY_RANGE)
        );

        entities.attack_mode[0] = GAME_ENTITY_ATTACK_RANGED;
        let ranged = entities.clip_for_state(&PACED_ATTACK, 0);
        assert_eq!(
            (ranged.clip, ranged.speed_q8, ranged.frame_range),
            (12, 640, CharacterActionFrameRange::FULL)
        );
    }

    #[test]
    fn authored_stagger_plays_at_its_speed_until_recovery_finishes() {
        static SLOW_STUN: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            stagger_speed_q8: 256,
            stagger_frame_range: CharacterActionFrameRange { start: 0, end: 42 },
            stagger_ticks: 85,
            // Knockdown off: a break then staggers, which is what this pins.
            character: psx_level::CharacterIndex(BODY_NO_KNOCKDOWN),
            ..test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED)
        }];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&SLOW_STUN);
        entities.set_spatial_active_mask(u64::MAX);
        entities.apply_hit(&SLOW_STUN, 0, VitalityChannelId::One, 10, 60);
        let clip = entities.clip_for_state(&SLOW_STUN, 0);
        assert_eq!(clip.speed_q8, 256);
        assert_eq!(clip.frame_range, SLOW_STUN[0].stagger_frame_range);
        for _ in 0..84 {
            entities.tick(&SLOW_STUN, far_input(), &mut OpenWorld);
            assert_eq!(entities.state(0), GameEntityState::Staggered);
        }
        assert_eq!(entities.clip_for_state(&SLOW_STUN, 0).phase_ticks, 84);
        entities.tick(&SLOW_STUN, far_input(), &mut OpenWorld);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
    }

    #[test]
    fn hits_break_poise_then_kill() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        // Poise pool is 50: 60 poise damage breaks it, a stagger.
        let outcome = entities.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 10, 60);
        assert_eq!(entities.state(0), GameEntityState::Staggered);
        assert_eq!(entities.health(0), 90);
        assert!(outcome.connected && outcome.staggered && !outcome.knocked_down && !outcome.died);
        for _ in 0..GAME_ENTITY_STAGGER_TICKS {
            entities.tick(&IDLE_ENEMY, far_input(), &mut OpenWorld);
        }
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // Past the break grace, a crit's break knocks it down; it gets up
        // into Aggro after the record's knockdown ticks.
        for _ in 0..crate::combat_flow::BREAK_GRACE_TICKS {
            entities.tick(&IDLE_ENEMY, far_input(), &mut OpenWorld);
        }
        let outcome = entities.receive_hit(
            &IDLE_ENEMY,
            0,
            player_strike(HitKind::Melee, VitalityChannelId::One, 10, 60, true),
        );
        assert_eq!(entities.state(0), GameEntityState::KnockedDown);
        assert!(outcome.connected && outcome.staggered && outcome.knocked_down);
        for _ in 0..TEST_COMBAT.knockdown_ticks {
            entities.tick(&IDLE_ENEMY, far_input(), &mut OpenWorld);
        }
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // Lethal damage kills; dead entities stop thinking.
        let outcome = entities.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 200, 0);
        assert!(outcome.connected && outcome.died);
        assert_eq!(entities.state(0), GameEntityState::Dead);
        let stats = entities.tick(&IDLE_ENEMY, near_input(), &mut OpenWorld);
        assert_eq!(stats.thought, 0);
        // A dead entity refuses further hits.
        assert_eq!(
            entities.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 10, 10),
            GameEntityHitOutcome::MISS
        );
    }

    /// The shipped cortex enemy shape: the two old 50-point pools as one
    /// 100 bar.
    static EVEN_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        ..test_record(
            1000,
            1000,
            0,
            512,
            game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
        )
    }];

    /// The old two-pool test actor (60 and 40) as its one 100 bar.
    static DUAL_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        ..test_record(
            1000,
            1000,
            0,
            512,
            game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
        )
    }];

    #[test]
    fn spawn_fills_the_health_bar() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        assert_eq!(entities.health(0), 100);
    }

    #[test]
    fn matching_hits_land_as_authored_and_opposite_hits_break_posture_sooner() {
        // IDLE_ENEMY is on Horizon (One) with a 50 poise pool.
        let mut matching = GameEntities::<8>::EMPTY;
        matching.spawn_from_records(&IDLE_ENEMY);
        // Two 40-poise matching swings: 40 does not break 50, 80 does.
        let first = matching.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 20, 40);
        let second = matching.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 20, 40);
        assert!(first.connected && !first.staggered && second.staggered);

        let mut opposite = GameEntities::<8>::EMPTY;
        opposite.spawn_from_records(&IDLE_ENEMY);
        opposite.combatants[0]
            .stance
            .set_active(VitalityChannelId::Two);
        // 40 x 1.25 = 50 breaks the 50 pool on the first swing.
        let first = opposite.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 20, 40);
        assert!(first.staggered);
    }

    #[test]
    fn matching_hits_land_as_authored_and_opposite_hits_take_the_multiplier() {
        let mut matching = GameEntities::<8>::EMPTY;
        matching.spawn_from_records(&DUAL_ENEMY);
        let hit = matching.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 40, 0);
        assert!(hit.connected && !hit.died);
        assert_eq!(matching.health(0), 60);
        for damage in [0, 1, 16, 38, 999] {
            assert_eq!(
                matching.scaled_stance_damage(&DUAL_ENEMY, 0, VitalityChannelId::One, damage),
                damage,
                "authored {damage}"
            );
        }
        assert_eq!(
            matching.scaled_stance_damage(&DUAL_ENEMY, 0, VitalityChannelId::Two, 38),
            47
        );

        let mut opposite = GameEntities::<8>::EMPTY;
        opposite.spawn_from_records(&DUAL_ENEMY);
        let hit = opposite.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 40, 0);
        assert!(hit.connected && !hit.died);
        // 1.25x = 50 off the one bar.
        assert_eq!(opposite.health(0), 50);
    }

    #[test]
    fn guard_cooldown_survives_repeated_recovery_attempts_at_both_cadences() {
        for delta in [1, 2] {
            let mut e = GameEntities::<8>::EMPTY;
            e.spawn_from_records(&DUAL_ENEMY);
            e.set_swap_cooldown_for_tests(900);
            e.mutate_stance(0);
            assert_eq!(e.stance_swap_cooldown(0), 900);
            for elapsed in (delta..900).step_by(usize::from(delta)) {
                e.tick_delta(&DUAL_ENEMY, far_input(), &mut OpenWorld, delta);
                e.mutate_stance(0);
                assert_eq!(
                    e.stance(0),
                    VitalityChannelId::Two,
                    "early swap at {elapsed}"
                );
            }
            e.tick_delta(&DUAL_ENEMY, far_input(), &mut OpenWorld, delta);
            e.mutate_stance(0);
            assert_eq!(e.stance(0), VitalityChannelId::One);
        }
    }

    #[test]
    fn a_poise_break_cancels_an_already_retained_attack_token() {
        let mut e = GameEntities::<8>::EMPTY;
        e.spawn_from_records(&IDLE_ENEMY);
        e.enter_state(
            0,
            GameEntityState::Attack,
            &mut GameEntityTickStats::default(),
        );
        let attack = e.deferred_attack(&IDLE_ENEMY[0], 0);
        assert!(e.deferred_attack_can_connect(attack));
        assert!(
            e.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 1, 50)
                .staggered
        );
        assert!(!e.deferred_attack_can_connect(attack));
        assert_eq!(e.state(0), GameEntityState::Staggered);
    }

    #[test]
    fn ranged_tell_tracks_then_commits_without_release_time_retargeting() {
        static ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            windup_ticks: 20,
            ..RANGED_ENEMY[0]
        }];
        let mut e = GameEntities::<8>::EMPTY;
        e.spawn_from_records(&ENEMY);
        let mut input = GameEntityTickInput {
            target: CombatTarget {
                position: [2500, 0, 1000],
                ..near_input().target
            },
        };
        begin_attack_at(&mut e, true, input);
        e.tick(&ENEMY, input, &mut OpenWorld);
        assert_eq!(e.state(0), GameEntityState::Windup);
        let initial_yaw = e.yaw(0);
        input.target.position = [2300, 0, 1600];
        for _ in 0..14 {
            e.tick(&ENEMY, input, &mut OpenWorld);
        }
        assert_ne!(e.yaw(0), initial_yaw);
        let velocity = e.ranged_velocity(0, [1000, 500, 1000], 160);
        let yaw = e.yaw(0);
        input.target.position = [0, 1000, 0];
        for _ in 0..10 {
            e.tick(&ENEMY, input, &mut OpenWorld);
        }
        assert_eq!(e.yaw(0), yaw);
        assert_eq!(e.ranged_velocity(0, [1000, 500, 1000], 160), velocity);
    }

    #[test]
    fn ranged_aim_converges_from_both_sides_of_the_body() {
        let mut e = GameEntities::<8>::EMPTY;
        e.spawn_from_records(&RANGED_ENEMY);
        e.set_axis(0, 0, 0);
        e.set_axis(0, 2, -256);
        e.set_yaw(0, 2048);
        e.capture_ranged_aim(
            0,
            GameEntityTickInput {
                target: CombatTarget {
                    position: [0, 0, -384],
                    height: 64,
                    ..near_input().target
                },
            },
        );
        assert_eq!(e.ranged_target(0), Some([0, 48, -384]));
        assert!(e.ranged_velocity(0, [20, 87, -333], 10)[0] < 0);
        assert!(e.ranged_velocity(0, [-20, 87, -333], 10)[0] > 0);
        assert_eq!(e.ranged_target(99), None);
    }

    #[test]
    fn ranged_aim_keeps_the_limited_turn_and_freezes_a_world_point() {
        let mut e = GameEntities::<8>::EMPTY;
        e.spawn_from_records(&RANGED_ENEMY);
        e.set_axis(0, 0, 0);
        e.set_axis(0, 2, 0);
        e.set_yaw(0, 0);
        // A player behind the enemy does not make an instantaneous rear shot.
        e.capture_ranged_aim(
            0,
            GameEntityTickInput {
                target: CombatTarget {
                    position: [0, 0, -128],
                    height: 64,
                    ..near_input().target
                },
            },
        );
        assert_eq!(e.ranged_target(0), Some([0, 48, 128]));
        e.set_axis(0, 0, 40);
        e.set_yaw(0, 1024);
        assert_eq!(e.ranged_target(0), Some([0, 48, 128]));
    }

    #[test]
    fn authored_charge_tracks_late_but_release_and_stagger_close_it() {
        let mut e = GameEntities::<8>::EMPTY;
        e.spawn_from_records(&RANGED_ENEMY);
        let input = GameEntityTickInput {
            target: CombatTarget {
                position: [2500, 0, 1000],
                ..near_input().target
            },
        };
        begin_attack_at(&mut e, true, input);
        for _ in 0..40 {
            e.tick(&RANGED_ENEMY, input, &mut OpenWorld);
            if e.state(0) == GameEntityState::Attack {
                break;
            }
        }
        assert_eq!(e.state(0), GameEntityState::Attack);
        let initial = e.yaw(0);
        e.track_authored_ranged_charge(0, [2000, 0, 1800], 1024, 2);
        assert_ne!(e.yaw(0), initial);
        let shot = e.deferred_attack(&RANGED_ENEMY[0], 0);
        assert!(e.commit_deferred_projectile(shot, 0));
        let velocity = e.ranged_velocity(0, [1000, 500, 1000], 160);
        e.track_authored_ranged_charge(0, [0, 0, 0], 1024, 2);
        assert_eq!(e.ranged_velocity(0, [1000, 500, 1000], 160), velocity);
        e.enter_state(
            0,
            GameEntityState::Staggered,
            &mut GameEntityTickStats::default(),
        );
        e.track_authored_ranged_charge(0, [0, 0, 0], 1024, 2);
        assert_eq!(e.ranged_velocity(0, [1000, 500, 1000], 160), velocity);
    }

    #[test]
    fn deliberate_stance_swap_reports_a_twelve_tick_tell() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        assert_eq!(entities.stance(0), VitalityChannelId::One);
        assert_eq!(entities.stance_swap_progress_q12(0), 4096);

        entities.enter_state(
            0,
            GameEntityState::Recover,
            &mut GameEntityTickStats::default(),
        );
        assert_eq!(
            entities.stance(0),
            VitalityChannelId::One,
            "recovery does not swap automatically"
        );
        entities.mutate_stance(0);
        assert_eq!(entities.stance(0), VitalityChannelId::Two);
        assert_eq!(entities.stance_swap_progress_q12(0), 0);
        assert!(entities.stance_swap_in_progress(0));

        entities.advance_stance_swap(0, 6);
        assert_eq!(entities.stance_swap_progress_q12(0), 2048);
        entities.advance_stance_swap(0, 6);
        assert_eq!(entities.stance_swap_progress_q12(0), 4096);
        assert!(!entities.stance_swap_in_progress(0));
    }

    #[test]
    fn eye_pulse_is_one_shot_and_outlives_the_palette_swap() {
        let mut e = GameEntities::<8>::EMPTY;
        e.spawn_from_records(&DUAL_ENEMY);
        assert_eq!(e.stance_eye_pulse_q12(0), None);
        assert_eq!(e.stance_eye_pulse_q12(99), None);
        e.set_swap_cooldown_for_tests(60);
        e.mutate_stance(0);
        assert_eq!(e.stance_eye_pulse_q12(0), Some(0));
        e.advance_stance_swap(0, 12);
        assert!(!e.stance_swap_in_progress(0));
        assert!(e.stance_eye_pulse_q12(0).is_some());
        let before = e.stance_eye_pulse_q12(0);
        e.mutate_stance(0); // Cooldown rejection must not flash again.
        assert_eq!(e.stance_eye_pulse_q12(0), before);
        e.advance_stance_swap(0, 18);
        assert_eq!(e.stance_eye_pulse_q12(0), None);
        e.combatants[0].stance.set_swap_cooldown(0);
        e.mutate_stance(0);
        assert_eq!(e.stance(0), VitalityChannelId::One);
        assert_eq!(e.stance_eye_pulse_q12(0), Some(0));
        e.enter_state(
            0,
            GameEntityState::Dead,
            &mut GameEntityTickStats::default(),
        );
        assert_eq!(e.stance_eye_pulse_q12(0), None);
        e.spawn_from_records(&DUAL_ENEMY);
        assert_eq!(e.stance_eye_pulse_q12(0), None);
    }

    /// A hit from either channel comes off the one bar; the channel only
    /// decides the stance multiplier.
    #[test]
    fn a_hit_of_either_channel_comes_off_the_one_bar() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 20, 0);
        assert_eq!(entities.health(0), 80);
        // Zenith is the opposite stance here (the actor is on Horizon), so
        // the 15 lands at 1.25x.
        entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 15, 0);
        assert_eq!(entities.health(0), 62);
    }

    /// A hit bigger than either old pool is just damage now: nothing spills,
    /// the bar takes it all, and the actor lives while health remains.
    #[test]
    fn a_hit_bigger_than_an_old_pool_is_just_damage() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        let outcome = entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 75, 0);
        assert_eq!(entities.health(0), 25);
        assert!(outcome.connected && !outcome.died);

        entities.spawn_from_records(&DUAL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        // Opposite stance: 55 x 1.25 = 68.
        entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 55, 0);
        assert_eq!(entities.health(0), 32);
    }

    /// Death is the one bar empty, from whichever channel the last point
    /// went.
    #[test]
    fn death_is_the_bar_empty() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        // Opposite stance: 40 x 1.25 = 50.
        let outcome = entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 40, 0);
        assert!(outcome.connected && !outcome.died);
        assert_ne!(entities.state(0), GameEntityState::Dead);
        assert_eq!(entities.health(0), 50);

        let outcome = entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 49, 0);
        assert!(!outcome.died);
        assert_eq!(entities.health(0), 1);

        let outcome = entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 1, 0);
        assert!(outcome.died);
        assert_eq!(entities.state(0), GameEntityState::Dead);
    }

    /// The bar is the old two pools summed, so a single-channel attacker
    /// kills the old 60/40 actor on the same 100 damage it took before the
    /// pools became one bar: time-to-kill did not move for it.
    #[test]
    fn one_channel_attacker_still_kills_at_the_summed_pool() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        for _ in 0..3 {
            assert!(
                !entities
                    .apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 25, 0)
                    .died
            );
        }
        assert!(
            entities
                .apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 25, 0)
                .died
        );
    }

    /// One overwhelming hit must kill outright, whatever its width: any
    /// narrowing on the way through would leave the actor standing. Reported
    /// from live combat (against the old two-pool spill).
    #[test]
    fn one_overwhelming_hit_kills_outright() {
        for damage in [u16::MAX, u16::MAX - 1, 60_000, 101, 100] {
            let mut entities = GameEntities::<8>::EMPTY;
            entities.spawn_from_records(&EVEN_ENEMY);
            entities.set_spatial_active_mask(u64::MAX);
            let outcome = entities.apply_hit(&EVEN_ENEMY, 0, VitalityChannelId::One, damage, 0);
            assert!(
                outcome.connected && outcome.died,
                "{damage} damage against a 100 bar must kill, got {outcome:?}"
            );
            assert_eq!(entities.state(0), GameEntityState::Dead, "damage {damage}");
            assert_eq!(entities.health(0), 0, "damage {damage}");
        }
    }

    /// One short of the bar must leave exactly one point standing, which pins
    /// the death rule against an off-by-one in either direction.
    #[test]
    fn one_short_of_the_bar_does_not_kill() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&EVEN_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        let outcome = entities.apply_hit(&EVEN_ENEMY, 0, VitalityChannelId::One, 99, 0);
        assert!(outcome.connected && !outcome.died);
        assert_eq!(entities.health(0), 1);
        assert_ne!(entities.state(0), GameEntityState::Dead);
        // And the last point finishes it.
        assert!(
            entities
                .apply_hit(&EVEN_ENEMY, 0, VitalityChannelId::Two, 1, 0)
                .died
        );
    }

    /// The same overwhelming hit from the Zenith side, so a width bug cannot
    /// hide behind the channel that happens to be consumed first.
    #[test]
    fn an_overwhelming_zenith_hit_kills_outright() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&EVEN_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        assert!(
            entities
                .apply_hit(&EVEN_ENEMY, 0, VitalityChannelId::Two, u16::MAX, 0)
                .died
        );
        assert_eq!(entities.health(0), 0);
    }

    /// The same overwhelming hit through the arc sweep, which is the other
    /// public way damage reaches an actor. A width bug in the
    /// wrapper would be invisible to the direct `apply_hit` tests above.
    #[test]
    fn an_overwhelming_arc_hit_kills_outright() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&EVEN_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        // Put the enemy on Zenith so the Horizon (One) swing is an
        // opposite-stance hit; the test is about arc width, not the rule.
        entities.combatants[0]
            .stance
            .set_active(VitalityChannelId::Two);
        let arc = MeleeArc {
            room: RoomIndex(0),
            x: 1000,
            z: 800,
            yaw: 1024,
            reach: 400,
            half_angle: 1024,
        };
        let mut swing = 0u64;
        let stats = entities.apply_melee_arc(
            &EVEN_ENEMY,
            &arc,
            VitalityChannelId::One,
            u16::MAX,
            0,
            &mut swing,
        );
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.deaths, 1);
        assert_eq!(entities.health(0), 0);
    }

    #[test]
    fn an_enemy_mid_swap_lets_the_players_swing_pass_through() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&EVEN_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        let arc = MeleeArc {
            room: RoomIndex(0),
            x: 1000,
            z: 800,
            yaw: 1024,
            reach: 400,
            half_angle: 1024,
        };
        let full = entities.health(0);
        entities.mutate_stance(0);
        // Past the parry window (its own test pins that part).
        let parry = TEST_COMBAT.parry_ticks;
        entities.advance_stance_swap(0, parry + 1);
        let mut swing = 0u64;
        for _ in parry + 1..=u16::from(TEST_SWAP_INVULNERABLE_TICKS) {
            assert!(entities.invulnerable(&EVEN_ENEMY, 0));
            assert!(entities.passes_through(&EVEN_ENEMY, 0));
            let stats = entities.apply_melee_arc(
                &EVEN_ENEMY,
                &arc,
                VitalityChannelId::One,
                10,
                0,
                &mut swing,
            );
            assert_eq!(stats.hits, 0, "the swing passes through");
            assert_eq!(swing, 0, "and is not used up");
            entities.advance_stance_swap(0, 1);
        }
        assert_eq!(entities.health(0), full);
        // The window is the record's data, then the same swing lands.
        assert!(!entities.invulnerable(&EVEN_ENEMY, 0));
        let stats =
            entities.apply_melee_arc(&EVEN_ENEMY, &arc, VitalityChannelId::One, 10, 0, &mut swing);
        assert_eq!(stats.hits, 1);
        assert_eq!(swing, 1);
        assert!(entities.health(0) < full);
    }

    #[test]
    fn a_non_swapping_enemy_is_always_hittable() {
        // Melee-only: under stance-bound attacks it always wants Horizon, so
        // it never swaps and never enters the window.
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        for _ in 0..240 {
            entities.tick(&IDLE_ENEMY, near_input(), &mut OpenWorld);
            assert!(!entities.invulnerable(&IDLE_ENEMY, 0));
            assert_eq!(entities.stance(0), VitalityChannelId::One);
        }
        let out = entities.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 1, 0);
        assert!(out.connected);
    }

    #[test]
    fn an_enemy_reads_its_swap_window_from_its_own_record() {
        static SHORT: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            character: psx_level::CharacterIndex(BODY_SHORT_SWAP),
            ..EVEN_ENEMY[0]
        }];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&SHORT);
        entities.mutate_stance(0);
        entities.advance_stance_swap(0, 2);
        assert!(entities.invulnerable(&SHORT, 0));
        entities.advance_stance_swap(0, 1);
        assert!(!entities.invulnerable(&SHORT, 0));
        assert!(
            entities.invulnerable(&EVEN_ENEMY, 0),
            "the same clock under a 10-tick record is still protected"
        );
    }

    /// Commit slot 0 to an attack (the controller's decision, made here
    /// directly): its Windup starts now.
    fn begin_attack(entities: &mut GameEntities<8>, ranged: bool) {
        begin_attack_at(entities, ranged, near_input());
    }

    /// [`begin_attack`] at `input`'s target: faces it and, for a shot,
    /// takes its aim point.
    fn begin_attack_at(entities: &mut GameEntities<8>, ranged: bool, input: GameEntityTickInput<'_>) {
        let target = input.target.position;
        let dx = target[0] - entities.px(0);
        let dz = target[2] - entities.pz(0);
        entities.set_yaw(0, (atan2_q12(dx, dz) & GAME_ENTITY_YAW_MASK) as i16);
        entities.select_attack(0, ranged);
        if ranged {
            entities.capture_ranged_aim(0, input);
        }
        entities.enter_state(0, GameEntityState::Windup, &mut GameEntityTickStats::default());
    }

    /// Drive IDLE_ENEMY into its Attack window against the near-input
    /// player (windup_ticks = 3): Windup -> 3 windup ticks -> Attack.
    fn advance_into_attack(entities: &mut GameEntities<8>, input: GameEntityTickInput<'_>) {
        begin_attack(entities, false);
        for _ in 0..3 {
            entities.tick(&IDLE_ENEMY, input, &mut OpenWorld);
        }
        assert_eq!(entities.state(0), GameEntityState::Attack);
    }

    #[test]
    fn attack_window_damages_the_player_once_per_swing() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        advance_into_attack(&mut entities, near_input());
        // First active tick connects with the record's touch damage.
        let stats = entities.tick(&IDLE_ENEMY, near_input(), &mut OpenWorld);
        assert_eq!(stats.player_hits, 1);
        assert_eq!(stats.player_damage, IDLE_ENEMY[0].touch_damage);
        // The rest of the window and the recovery stay dry: one
        // swing, one connection. (The NEXT windup->attack loop may
        // legitimately connect again, so only the dry span is
        // walked.)
        let mut later_damage = 0u16;
        for _ in 0..8 {
            later_damage += entities
                .tick(&IDLE_ENEMY, near_input(), &mut OpenWorld)
                .player_damage;
        }
        assert_eq!(later_damage, 0);
        assert_eq!(entities.state(0), GameEntityState::Recover);
    }

    #[test]
    fn i_frames_whiff_the_swing_but_the_tail_still_bites() {
        // Fully i-framed window: no contact at all.
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        advance_into_attack(&mut entities, near_input());
        let rolling_player = Combatant::EMPTY;
        let rolling = GameEntityTickInput {
            target: CombatTarget {
                defender: Defender {
                    combatant: &rolling_player,
                    swap_invulnerable_ticks: 0,
                    parry_ticks: 0,
                    action: ActionProtection {
                        authored: None,
                        dodge: true,
                    },
                },
                stance: None,
                ..near_input().target
            },
        };
        let mut damage = 0u16;
        for _ in 0..GAME_ENTITY_ATTACK_ACTIVE_TICKS {
            damage += entities
                .tick(&IDLE_ENEMY, rolling, &mut OpenWorld)
                .player_damage;
        }
        assert_eq!(damage, 0);
        assert_eq!(entities.state(0), GameEntityState::Recover);

        // I-framing only the first half leaves the tail live: rolling
        // too early still gets clipped (souls timing rules).
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        advance_into_attack(&mut entities, near_input());
        let early_roll = entities.tick(&IDLE_ENEMY, rolling, &mut OpenWorld);
        assert_eq!(early_roll.player_hits, 0);
        let tail = entities.tick(&IDLE_ENEMY, near_input(), &mut OpenWorld);
        assert_eq!(tail.player_hits, 1);
    }

    #[test]
    fn attacks_whiff_behind_the_committed_facing() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        // Commit the windup against a player at +X (facing locks to
        // 1024)...
        advance_into_attack(&mut entities, near_input());
        // ...then the player rolls PAST the body to -X: same reach,
        // outside the front arc, outside the point-blank ring.
        let behind = GameEntityTickInput {
            target: CombatTarget {
                position: [800, 0, 1000],
                ..near_input().target
            },
        };
        let mut damage = 0u16;
        for _ in 0..GAME_ENTITY_ATTACK_ACTIVE_TICKS {
            damage += entities
                .tick(&IDLE_ENEMY, behind, &mut OpenWorld)
                .player_damage;
        }
        assert_eq!(damage, 0);
        assert_eq!(entities.state(0), GameEntityState::Recover);
    }

    #[test]
    fn two_tick_delta_preserves_patrol_speed_and_state_clock() {
        let mut stepped = GameEntities::<8>::EMPTY;
        let mut batched = GameEntities::<8>::EMPTY;
        stepped.spawn_from_records(&PATROL_ENEMY);
        batched.spawn_from_records(&PATROL_ENEMY);
        stepped.state[0] = GameEntityState::Patrol as u8;
        batched.state[0] = GameEntityState::Patrol as u8;

        stepped.tick(&PATROL_ENEMY, far_input(), &mut OpenWorld);
        stepped.tick(&PATROL_ENEMY, far_input(), &mut OpenWorld);
        batched.tick_delta(&PATROL_ENEMY, far_input(), &mut OpenWorld, 2);

        assert_eq!(batched.position(0), stepped.position(0));
        assert_eq!(batched.yaw(0), stepped.yaw(0));
        assert_eq!(batched.state_ticks[0], stepped.state_ticks[0]);
    }

    #[test]
    fn occluded_melee_arc_blocks_without_latching_the_swing_bit() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        // The enemy is on Horizon, so the Horizon (One) swings below match
        // its stance and land as authored; these tests are about arc
        // mechanics, not the stance rule.
        let arc = MeleeArc {
            room: RoomIndex(0),
            x: 1200,
            z: 1000,
            yaw: 3072,
            reach: 640,
            half_angle: 683,
        };
        // psx-numeric-allow-next-line: swing bitmask scratch in tests
        let mut swing = 0u64;

        // A wall between the actors: no hit, no damage, and crucially the
        // swing bit stays clear so the same swing can connect later.
        let stats = entities.apply_melee_arc_occluded(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            10,
            40,
            false,
            &mut swing,
            |_, _| true,
        );
        assert_eq!(stats, MeleeArcStats::default());
        assert_eq!(entities.health(0), 100);
        assert_eq!(swing, 0);

        // The occluder clears (door finished opening): the identical swing
        // connects exactly once and the closure sees the live position.
        let mut probed = None;
        let stats = entities.apply_melee_arc_occluded(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            10,
            40,
            false,
            &mut swing,
            |entity, position| {
                probed = Some((entity, position));
                false
            },
        );
        assert_eq!(
            stats,
            MeleeArcStats {
                hits: 1,
                staggers: 0,
                deaths: 0,
                parries: 0
            }
        );
        assert_eq!(entities.health(0), 90);
        assert_eq!(probed, Some((0, entities.position(0))));
        assert_ne!(swing, 0);
    }

    #[test]
    fn melee_arc_hits_once_per_swing_and_skips_dead_and_other_rooms() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        // The enemy is on Horizon, so the Horizon (One) swings below match
        // its stance and land as authored; these tests are about arc
        // mechanics, not the stance rule.
        // Player at (1200, 1000) facing -X: the enemy at (1000, 1000)
        // sits dead ahead, 200 units out.
        let arc = MeleeArc {
            room: RoomIndex(0),
            x: 1200,
            z: 1000,
            yaw: 3072,
            reach: 640,
            half_angle: 683,
        };
        // psx-numeric-allow-next-line: swing bitmask scratch in tests
        let mut swing = 0u64;
        // Swing 1: connects (poise 40 <= pool 50, no stagger)...
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            20,
            40,
            &mut swing,
        );
        assert_eq!(
            stats,
            MeleeArcStats {
                hits: 1,
                staggers: 0,
                deaths: 0,
                parries: 0
            }
        );
        assert_eq!(entities.health(0), 80);
        // ...and the same swing never double-taps.
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            20,
            40,
            &mut swing,
        );
        assert_eq!(stats, MeleeArcStats::default());
        // Swing 2: accumulated poise (40 + 40) breaks the 50 pool.
        swing = 0;
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            20,
            40,
            &mut swing,
        );
        assert_eq!(stats.staggers, 1);
        assert_eq!(entities.state(0), GameEntityState::Staggered);
        // Swing 3 (health 60 - 50 = 10), swing 4 kills.
        swing = 0;
        entities.apply_melee_arc(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            50,
            40,
            &mut swing,
        );
        swing = 0;
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            10,
            40,
            &mut swing,
        );
        assert_eq!(stats.deaths, 1);
        assert_eq!(entities.state(0), GameEntityState::Dead);
        // Dead entities are no longer targets.
        swing = 0;
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            10,
            40,
            &mut swing,
        );
        assert_eq!(stats, MeleeArcStats::default());

        // Wrong-room arcs never connect (cooked positions are
        // room-local; a same-coordinate player in another room is an
        // alias, not a neighbor).
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        swing = 0;
        let wrong_room = MeleeArc {
            room: RoomIndex(2),
            ..arc
        };
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &wrong_room,
            VitalityChannelId::One,
            10,
            40,
            &mut swing,
        );
        assert_eq!(stats, MeleeArcStats::default());
        assert_eq!(entities.health(0), 100);

        // Facing away whiffs once the target is outside the
        // point-blank ring: from 600 units out (ring is 192 + 64),
        // facing +X misses the enemy at -X, facing -X connects.
        swing = 0;
        let away = MeleeArc {
            x: 1600,
            yaw: 1024,
            ..arc
        };
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &away,
            VitalityChannelId::One,
            10,
            40,
            &mut swing,
        );
        assert_eq!(stats, MeleeArcStats::default());
        swing = 0;
        let toward = MeleeArc { yaw: 3072, ..away };
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &toward,
            VitalityChannelId::One,
            10,
            40,
            &mut swing,
        );
        assert_eq!(stats.hits, 1);
    }

    /// Deferred twin of [`advance_into_attack`]: same grammar, tokens routed
    /// through `attacks`.
    fn advance_into_attack_deferred(
        entities: &mut GameEntities<8>,
        attacks: &mut DeferredGameEntityAttacks<8>,
    ) {
        begin_attack(entities, false);
        for _ in 0..3 {
            entities.tick_delta_deferred(&IDLE_ENEMY, near_input(), &mut OpenWorld, 1, attacks);
        }
        assert_eq!(entities.state(0), GameEntityState::Attack);
        // The Windup -> Attack transition tick runs the Windup arm; the
        // first token appears on the first ACTIVE tick, not here.
        assert!(attacks.is_empty());
    }

    #[test]
    fn authored_melee_tell_turns_at_bounded_speed_and_stops_after_contact_or_stagger() {
        let mut entities = GameEntities::<8>::EMPTY;
        let mut attacks = DeferredGameEntityAttacks::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        advance_into_attack_deferred(&mut entities, &mut attacks);
        entities.set_yaw(0, 0);
        entities.track_authored_melee_tell(0, [2000, 0, 1000], 2);
        assert_eq!(entities.yaw_raw(0), 32);
        entities.authored_connection_mask[0] = 1;
        entities.track_authored_melee_tell(0, [2000, 0, 1000], 2);
        assert_eq!(entities.yaw_raw(0), 32);
        entities.authored_connection_mask[0] = 0;
        entities.state[0] = GameEntityState::Staggered as u8;
        entities.track_authored_melee_tell(0, [2000, 0, 1000], 2);
        assert_eq!(entities.yaw_raw(0), 32);
    }

    #[test]
    fn deferred_tokens_freeze_active_boundary_frames_and_recover_is_dry() {
        let mut entities = GameEntities::<8>::EMPTY;
        let mut attacks = DeferredGameEntityAttacks::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        advance_into_attack_deferred(&mut entities, &mut attacks);
        let windup = u16::from(IDLE_ENEMY[0].windup_ticks);

        // Every active tick freezes exactly one token whose clip/phase is the
        // attack one-shot the pose overrides play, and the deferred tick
        // never applies immediate player damage.
        for active_tick in 1..=GAME_ENTITY_ATTACK_ACTIVE_TICKS {
            let stats = entities.tick_delta_deferred(
                &IDLE_ENEMY,
                near_input(),
                &mut OpenWorld,
                1,
                &mut attacks,
            );
            assert_eq!(stats.attacking, 1);
            assert_eq!(stats.player_hits, 0);
            assert_eq!(stats.player_damage, 0);
            assert_eq!(attacks.len(), 1);
            let token = attacks.get(0).unwrap();
            assert_eq!(token.entity(), 0);
            assert_eq!(token.room(), RoomIndex(0));
            assert_eq!(token.clip().clip, IDLE_ENEMY[0].attack_clip);
            assert!(token.clip().one_shot);
            assert_eq!(token.clip().phase_ticks, windup + active_tick);
            assert!(entities.deferred_attack_can_connect(token));
        }

        // The final active tick froze its token BEFORE transitioning to
        // Recover, so the boundary frame still resolves against the retained
        // attack pose even though the live state moved on.
        assert_eq!(entities.state(0), GameEntityState::Recover);
        let boundary = attacks.get(0).unwrap();
        assert_eq!(
            boundary.clip().phase_ticks,
            windup + GAME_ENTITY_ATTACK_ACTIVE_TICKS
        );
        assert!(entities.connect_deferred_attack(boundary));

        // The first Recover tick emits nothing.
        let stats = entities.tick_delta_deferred(
            &IDLE_ENEMY,
            near_input(),
            &mut OpenWorld,
            1,
            &mut attacks,
        );
        assert_eq!(stats.attacking, 0);
        assert!(attacks.is_empty());
    }

    #[test]
    fn encounter_clear_requires_every_enabled_enemy_and_resets_on_respawn() {
        let mut entities = GameEntities::<8>::EMPTY;
        assert!(!entities.encounter_cleared(&[]));
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        assert!(!entities.encounter_cleared(&IDLE_ENEMY));
        entities.state[0] = GameEntityState::Dead as u8;
        assert!(entities.encounter_cleared(&IDLE_ENEMY));
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        assert!(!entities.encounter_cleared(&IDLE_ENEMY));
        let mut disabled = IDLE_ENEMY;
        disabled[0].flags &= !game_entity_flags::ENABLED;
        assert!(!entities.encounter_cleared(&disabled));
        entities.state[0] = GameEntityState::Dead as u8;
        entities.overflow = 1;
        assert!(!entities.encounter_cleared(&IDLE_ENEMY));
    }

    #[test]
    fn melee_combo_windows_are_independent_and_interruptible() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        let mut stats = GameEntityTickStats::default();
        entities.enter_state(0, GameEntityState::Attack, &mut stats);
        let token = entities.deferred_attack(&IDLE_ENEMY[0], 0);
        assert_eq!(entities.deferred_melee_hit_mask(token), Some(0));
        assert!(entities.connect_deferred_melee_window(token, 1));
        assert!(!entities.connect_deferred_melee_window(token, 1));
        assert!(entities.connect_deferred_melee_window(token, 2));
        assert!(entities.connect_deferred_melee_window(token, 4));
        assert_eq!(entities.deferred_melee_hit_mask(token), Some(7));
        assert!(!entities.connect_deferred_melee_window(token, 0));
        entities.enter_state(0, GameEntityState::Staggered, &mut stats);
        assert!(!entities.connect_deferred_melee_window(token, 8));
        entities.enter_state(0, GameEntityState::Attack, &mut stats);
        let fresh = entities.deferred_attack(&IDLE_ENEMY[0], 0);
        assert_eq!(entities.deferred_melee_hit_mask(fresh), Some(0));
        assert!(!entities.connect_deferred_melee_window(token, 8));
        assert!(entities.connect_deferred_melee_window(fresh, 1));
        entities.enter_state(0, GameEntityState::Dead, &mut stats);
        assert!(!entities.connect_deferred_melee_window(fresh, 2));
    }

    #[test]
    fn ranged_volley_latches_each_emitter_and_interrupts_cancel_pending_shots() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        entities.attack_mode[0] = GAME_ENTITY_ATTACK_RANGED;
        let mut stats = GameEntityTickStats::default();
        entities.enter_state(0, GameEntityState::Attack, &mut stats);
        let token = entities.deferred_attack(&IDLE_ENEMY[0], 0);
        assert_eq!(entities.deferred_projectile_release_mask(token), Some(0));
        assert!(entities.commit_deferred_projectile(token, 1));
        assert!(!entities.commit_deferred_projectile(token, 1));
        assert!(!entities.commit_deferred_projectile(token, 16));
        assert!(entities.commit_deferred_projectile(token, 3));
        assert_eq!(entities.deferred_projectile_release_mask(token), Some(10));
        entities.enter_state(0, GameEntityState::Staggered, &mut stats);
        assert!(!entities.commit_deferred_projectile(token, 4));
        entities.enter_state(0, GameEntityState::Attack, &mut stats);
        let fresh = entities.deferred_attack(&IDLE_ENEMY[0], 0);
        assert_eq!(entities.deferred_projectile_release_mask(fresh), Some(0));
        assert!(!entities.commit_deferred_projectile(token, 4));
        entities.enter_state(0, GameEntityState::Dead, &mut stats);
        assert!(!entities.commit_deferred_projectile(fresh, 4));
        let mut melee = fresh;
        melee.ranged = false;
        assert_eq!(entities.deferred_projectile_release_mask(melee), None);
    }

    #[test]
    fn deferred_tokens_reject_stale_generation_and_swing_sequence() {
        let mut entities = GameEntities::<8>::EMPTY;
        let mut attacks = DeferredGameEntityAttacks::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        advance_into_attack_deferred(&mut entities, &mut attacks);
        entities.tick_delta_deferred(&IDLE_ENEMY, near_input(), &mut OpenWorld, 1, &mut attacks);
        let token = attacks.get(0).unwrap();
        let player = [1200, 0, 1000];

        // A token from another swing of this entity fails closed even when
        // its generation is current.
        let stale_swing = DeferredGameEntityAttack {
            swing_sequence: token.swing_sequence.wrapping_sub(1),
            ..token
        };
        assert!(!entities.deferred_attack_can_connect(stale_swing));
        assert!(!entities.connect_deferred_attack(stale_swing));
        assert!(!entities.deferred_attack_legacy_arc_hits(&IDLE_ENEMY, stale_swing, player, 192,));

        // The next entity tick retires the previous tick's tokens wholesale:
        // contact may only finalize against the poses retained for the tick
        // that emitted the token.
        entities.tick_delta_deferred(&IDLE_ENEMY, near_input(), &mut OpenWorld, 1, &mut attacks);
        assert!(!entities.deferred_attack_can_connect(token));
        assert!(!entities.connect_deferred_attack(token));
        assert!(!entities.deferred_attack_legacy_arc_hits(&IDLE_ENEMY, token, player, 192,));
        let fresh = attacks.get(0).unwrap();
        assert!(entities.deferred_attack_can_connect(fresh));
    }

    #[test]
    fn deferred_connection_latches_once_and_suppresses_the_legacy_arc() {
        let mut entities = GameEntities::<8>::EMPTY;
        let mut attacks = DeferredGameEntityAttacks::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        entities.set_spatial_active_mask(u64::MAX);
        advance_into_attack_deferred(&mut entities, &mut attacks);
        entities.tick_delta_deferred(&IDLE_ENEMY, near_input(), &mut OpenWorld, 1, &mut attacks);
        let token = attacks.get(0).unwrap();
        let player = [1200, 0, 1000];

        // Legacy arc geometry (frozen origin/yaw/reach) agrees the player is
        // reachable before any connection, whiffs behind the committed
        // facing.
        assert!(entities.deferred_attack_legacy_arc_hits(&IDLE_ENEMY, token, player, 192,));
        assert!(!entities.deferred_attack_legacy_arc_hits(&IDLE_ENEMY, token, [800, 0, 1000], 192,));

        // An authored-capsule connection latches the swing: the same token
        // cannot finalize twice, and the legacy arc goes dead with it, so one
        // swing can never damage through both policies.
        assert!(entities.connect_deferred_attack(token));
        assert!(!entities.connect_deferred_attack(token));
        assert!(!entities.deferred_attack_legacy_arc_hits(&IDLE_ENEMY, token, player, 192,));

        // The latch spans the remaining active ticks of the SAME swing.
        entities.tick_delta_deferred(&IDLE_ENEMY, near_input(), &mut OpenWorld, 1, &mut attacks);
        let later = attacks.get(0).unwrap();
        assert_eq!(later.swing_sequence, token.swing_sequence);
        assert!(!entities.deferred_attack_can_connect(later));
        assert!(!entities.connect_deferred_attack(later));
    }

    // ---- slot ownership and stable spawn ids ----------------------------

    /// Records indexed by pool slot: the slot-indexed slice every method
    /// takes. Slot 1 is never used by the pool tests below.
    static POOL_RECORDS: [LevelGameEntityRecord; 3] = [
        test_record(
            1000,
            1000,
            0,
            512,
            game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
        ),
        test_record(0, 0, 0, 0, 0),
        test_record(
            4000,
            1000,
            0,
            512,
            game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
        ),
    ];

    #[test]
    fn a_pool_slot_is_named_by_a_stable_id_not_by_its_position() {
        let (a, b) = (psx_level::StableId(0xA), psx_level::StableId(0xB));
        let mut pool = GameEntities::<4>::EMPTY;
        assert!(pool.spawn_slot(0, a, &POOL_RECORDS[0]));
        assert!(pool.spawn_slot(2, b, &POOL_RECORDS[2]));
        assert_eq!(pool.count(), 3);
        assert_eq!((pool.slot_of(a), pool.slot_of(b)), (Some(0), Some(2)));
        assert_eq!(pool.spawn_id(1), psx_level::StableId::NONE);
        assert_eq!(pool.slot_of(psx_level::StableId(0xC)), None);
        assert_eq!(pool.slot_of(psx_level::StableId::NONE), None);
        // The pool is the active set: a slot past it is refused, not wrapped.
        assert!(!pool.spawn_slot(4, a, &POOL_RECORDS[0]));
        assert_eq!(pool.position(2)[0], 4000);
    }

    #[test]
    fn a_killed_spawn_stays_dead_when_its_room_leaves_and_comes_back() {
        use crate::dormant::{dormant_state, DormantStates};
        let b = psx_level::StableId(0xB);
        let mut pool = GameEntities::<4>::EMPTY;
        let mut dormant = DormantStates::<8>::EMPTY;
        pool.spawn_slot(2, b, &POOL_RECORDS[2]);
        assert_ne!(pool.state(2), GameEntityState::Dead);
        // Kill it.
        let hit = pool.apply_hit(&POOL_RECORDS, 2, VitalityChannelId::One, 1000, 0);
        assert!(hit.died);
        assert_eq!(pool.state(2), GameEntityState::Dead);
        // Its room leaves the active set: the slot empties, the table keeps it.
        pool.release_slot(2, &mut dormant);
        assert_eq!(pool.slot_of(b), None);
        assert_eq!(dormant.get(b) & dormant_state::DEAD, dormant_state::DEAD);
        // The room comes back and the spawn lands in a different slot: it
        // spawns dead, it is not resurrected by streaming.
        assert!(pool.spawn_slot_remembered(1, b, &POOL_RECORDS[2], &dormant));
        assert_eq!(pool.state(1), GameEntityState::Dead);
        assert_eq!(pool.health(1), 0);
        // An entity that was merely alive when it left comes back alive.
        let c = psx_level::StableId(0xC);
        pool.spawn_slot(3, c, &POOL_RECORDS[0]);
        pool.release_slot(3, &mut dormant);
        assert_eq!(dormant.get(c), 0);
        assert!(pool.spawn_slot_remembered(3, c, &POOL_RECORDS[0], &dormant));
        assert_ne!(pool.state(3), GameEntityState::Dead);
    }

    #[test]
    fn the_boot_path_names_every_slot_and_behaves_as_before() {
        let ids = [psx_level::StableId(77)];
        let mut named = GameEntities::<4>::EMPTY;
        named.spawn_from_records_with_ids(&IDLE_ENEMY, &ids);
        let mut plain = GameEntities::<4>::EMPTY;
        plain.spawn_from_records(&IDLE_ENEMY);
        assert_eq!(named.spawn_id(0), ids[0]);
        assert_eq!(plain.spawn_id(0), psx_level::StableId::NONE);
        // Naming a slot changes nothing about how its entity thinks.
        for _ in 0..30 {
            named.tick(&IDLE_ENEMY, near_input(), &mut OpenWorld);
            plain.tick(&IDLE_ENEMY, near_input(), &mut OpenWorld);
        }
        assert_eq!(named.position(0), plain.position(0));
        assert_eq!(named.state(0), plain.state(0));
    }

    #[test]
    fn an_entity_thinks_the_same_in_whichever_slot_its_id_lands() {
        let id = psx_level::StableId(0x1234);
        let mut low = GameEntities::<4>::EMPTY;
        low.spawn_slot(0, id, &POOL_RECORDS[0]);
        let mut high = GameEntities::<4>::EMPTY;
        // Same record, slot 2 of a records slice that holds it there.
        high.spawn_slot(2, id, &POOL_RECORDS[0]);
        static LOW: [LevelGameEntityRecord; 1] = [POOL_RECORDS[0]];
        static HIGH: [LevelGameEntityRecord; 3] =
            [POOL_RECORDS[1], POOL_RECORDS[1], POOL_RECORDS[0]];
        low.set_spatial_active_mask(u64::MAX);
        high.set_spatial_active_mask(u64::MAX);
        for _ in 0..60 {
            low.tick(&LOW, near_input(), &mut OpenWorld);
            high.tick(&HIGH, near_input(), &mut OpenWorld);
        }
        assert_eq!(low.position(0), high.position(2));
        assert_eq!(low.state(0), high.state(2));
    }
}
