//! Souls-like game-entity runtime (phase 3 of
//! docs/game-runtime-plan.md): SoA state over cooked
//! [`LevelGameEntityRecord`]s with per-archetype tick dispatch across
//! the souls state grammar (Idle / Patrol / Aggro / Windup / Attack /
//! Recover / Staggered / Dead), adopted from hl-psx's prop AI shape
//! with two deliberate differences: thinking gates on the
//! portal-expanded ACTIVE ROOM set instead of BSP PVS, and melee
//! windup/commit/punish is the first-class attack grammar.
//!
//! Movement is Character-bound and motor-honest (the phase-3 seam
//! note): speeds come from the cooked record's Character-derived
//! `walk_speed`/`run_speed` (patrol walks, chase runs), and every step
//! goes through a [`GameEntityMover`] the owning game backs with the
//! engine motor's [`commit_body_step`] -- the exact grid-collision
//! stand/slide/step rules the player uses. Blocked patrol and chase
//! movement adopts Quake's bounded eight-direction chase search:
//! persist a working direction, reconsider occasionally, and try the
//! turnaround last. This gives BSP-aware local routing without a
//! navmesh, heap allocation, or an unbounded search. Attack CONTACT
//! resolution is the combat slice (see [`crate::combat`]). Games with
//! retained actor poses use [`Self::tick_delta_deferred`] to freeze each active
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

use crate::combat::{arc_hits_circle, MeleeArc};
use crate::vitality::{DualVitality, VitalityChannelId, VitalityPool};
use psx_level::{
    game_entity_flags, CharacterActionFrameRange, CharacterAnimationAction, LevelGameEntityRecord,
    RoomIndex, CHARACTER_ACTION_SPEED_UNSCALED_Q8,
};
use psx_math::atan2_q12;

/// Souls behavior state for one entity. The all-zero pattern is
/// [`GameEntityState::Idle`], preserving the crate's zeroed-storage
/// discipline.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameEntityState {
    /// Holding position, scanning for the player.
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
    /// Poise broke; briefly helpless.
    Staggered = 6,
    /// Health reached zero (or the record spawned disabled).
    Dead = 7,
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
    /// Face the player and wait.
    Hold = 0,
    /// Close distance toward the player.
    Approach = 1,
    /// Orbit counter-clockwise around the player.
    CircleLeft = 2,
    /// Orbit clockwise around the player.
    CircleRight = 3,
    /// Back away while keeping the player faced.
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
/// player is within `record.radius + player_radius + MARGIN` in XZ.
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
const GAME_ENTITY_ATTACK_MELEE_CHASE: u8 = 1 << 6;
const GAME_ENTITY_ATTACK_NEXT_HEAVY: u8 = 1 << 7;

/// Enemy guard is packed into the spare bits of the one-hit-per-swing byte.
/// Bit zero remains the contact latch, bit seven is the guarded channel, and
/// bits one through four carry the short mutation tell. This costs no extra
/// per-entity RAM at the 64 actor cap.
const GAME_ENTITY_ATTACK_CONNECTED: u8 = 1;
const GAME_ENTITY_STANCE_SWAP_SHIFT: u8 = 1;
const GAME_ENTITY_STANCE_SWAP_MASK: u8 = 0b0001_1110;
const GAME_ENTITY_STANCE_ZENITH: u8 = 1 << 7;

/// Duration of the enemy guard-colour sweep in 60 Hz simulation ticks.
pub const GAME_ENTITY_STANCE_SWAP_DURATION_TICKS: u8 = 12;

/// Damage against the channel an enemy currently guards (Q12 = 0.5x).
pub const GAME_ENTITY_GUARDED_DAMAGE_Q12: u16 = 2048;

/// Damage against the channel an enemy leaves exposed (Q12 = 1.5x).
pub const GAME_ENTITY_OPPOSED_DAMAGE_Q12: u16 = 6144;

/// Front-arc half-width for entity attacks, PSX angle units
/// (60 degrees). The entity faced the player when it committed to
/// its windup; a player who rolls past the body during the swing
/// leaves this arc and the attack whiffs -- the souls punish loop.
pub const GAME_ENTITY_ATTACK_HALF_ANGLE: u16 = 683;

/// Ticks a poise break keeps the entity committed to its single authored stun
/// one-shot. The current longest enemy clip is 19 frames at 12 Hz; 96 NTSC
/// ticks plays it to completion and leaves one tenth of a second on its final
/// recovered pose before AI control resumes.
pub const GAME_ENTITY_STAGGER_TICKS: u16 = 96;

/// De-aggro leash: the player escaping `aggro_radius` times this
/// factor drops the entity back to its idle/patrol loop.
pub const GAME_ENTITY_LEASH_FACTOR: i32 = 2;

/// Quarter turn in the engine's 4096-unit yaw representation.
const GAME_ENTITY_QUARTER_TURN: u16 = 1024;

/// Half turn in the engine's 4096-unit yaw representation.
const GAME_ENTITY_HALF_TURN: u16 = 2048;

/// One eighth turn in the engine's 4096-unit yaw representation.
const GAME_ENTITY_DIRECTION_STEP: u16 = 512;

/// Mask for the engine's 4096-unit yaw representation.
const GAME_ENTITY_YAW_MASK: u16 = 4095;

/// Ignore sub-pixel facing jitter when deciding whether to present the
/// in-place turn animation (roughly 0.7 degrees in Q12 yaw).
const GAME_ENTITY_TURN_PRESENTATION_THRESHOLD: u16 = 8;

/// Keep the turn pose visible briefly after a snapped facing correction so a
/// single simulation-tick yaw change still reads as a planted pivot.
const GAME_ENTITY_TURN_PRESENTATION_TICKS: u8 = 48;

/// Quake's close-enough tolerance when choosing the direct chase axes.
const GAME_ENTITY_CHASE_AXIS_EPSILON: i32 = 10;

/// Collision directions evaluated in one simulation tick. Quake can scan all
/// eight immediately because its monster move uses a single cheap hull trace;
/// PSoXide's player-equivalent body step performs several stand/floor traces.
/// Spreading the same bounded search across four 30 Hz NPC ticks prevents one
/// blocked actor from consuming a visual frame.
const GAME_ENTITY_DIRECTION_PROBES_PER_TICK: u8 = 2;

/// Movement backend the owning game supplies per tick: one
/// collision-checked walk step for a body cylinder, in the entity's
/// own room-local space. The reference game backs this with the
/// engine motor's `commit_body_step` over the entity room's active
/// collision (grid floors, step rules, walls, prop blockers), so
/// entities move under exactly the player's movement rules.
pub trait GameEntityMover {
    /// Attempt to move the body of entity `entity` at `position`
    /// (feet anchor, room-local engine units, in `room`) by
    /// `(dx, dz)`. Returns the committed position: the target when
    /// free, an axis-slide when partially blocked, or `position`
    /// unchanged when fully blocked (including "the room's collision
    /// is not resident"). `entity` lets the backing collision exclude
    /// the mover's own body from its blocker set.
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

    /// Attempt one authored chase direction without inventing a second
    /// direction through axis sliding. The default preserves existing movers;
    /// BSP backends override this with Quake-style exact-direction hull motion.
    fn step_direction(
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

    /// Whether a point segment from an NPC's eye to the player's eye is clear
    /// of world geometry. Games without a BSP visibility provider retain the
    /// previous behavior; BSP-backed games override this and fail closed.
    fn line_of_sight(&mut self, _room: RoomIndex, _from: [i32; 3], _to: [i32; 3]) -> bool {
        true
    }
}

/// No-clip mover: commits every step verbatim. Host-test shape, and
/// the honest fallback for games without collision wired yet.
pub struct NoClipMover;

impl GameEntityMover for NoClipMover {
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
        [
            position[0].saturating_add(dx),
            position[1],
            position[2].saturating_add(dz),
        ]
    }
}

/// Per-tick inputs the owning game threads in: the player pose and
/// the portal-expanded active-room set the AI gating reads.
#[derive(Clone, Copy)]
pub struct GameEntityTickInput<'a> {
    /// Player position, world/room-local engine units (the same space
    /// the cooked records use).
    pub player: [i32; 3],
    /// Room containing the player.
    pub player_room: RoomIndex,
    /// Player body radius, engine units (the player Character's
    /// capsule; the other half of Character-derived attack reach).
    pub player_radius: i32,
    /// Player body height, used to trace sight at torso height instead of
    /// along the floor.
    pub player_height: i32,
    /// True while the player's motor action grants i-frames (roll /
    /// active roll invulnerability). Entity attacks resolve no
    /// contact against an invulnerable player -- the swing stays
    /// live, so i-framing only the first half of a window still eats
    /// the tail (souls timing rules).
    pub player_invulnerable: bool,
    /// Rooms currently in the active window (the portal-expanded
    /// set). Entities in other rooms and with no engaged behavior do
    /// not think this tick.
    pub active_rooms: &'a [RoomIndex],
}

/// Per-tick outcome counters, for overlays and budget telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameEntityTickStats {
    /// Entities that ran their state machine this tick.
    pub thought: u16,
    /// Entities skipped by the active-room gate.
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

/// What one [`GameEntities::apply_hit`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameEntityHitOutcome {
    /// The hit landed on a live entity.
    pub connected: bool,
    /// The hit broke poise (entity is now [`GameEntityState::Staggered`]).
    pub staggered: bool,
    /// The hit was lethal (entity is now [`GameEntityState::Dead`]).
    pub died: bool,
}

impl GameEntityHitOutcome {
    /// Out-of-range / already-dead target: nothing happened.
    pub const MISS: Self = Self {
        connected: false,
        staggered: false,
        died: false,
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
}

/// SoA runtime state for cooked game entities. Entity `i` mirrors
/// `records[i]` (spawn is 1:1, clamped to `MAX_ENTITIES`), so links
/// like `model_instance` stay index-stable.
pub struct GameEntities<const MAX_ENTITIES: usize> {
    /// Live entity count = `min(records.len(), MAX_ENTITIES)`.
    count: u16,
    /// Cooked records past `MAX_ENTITIES` that could not spawn.
    overflow: u16,
    /// Current position X, room-local engine units.
    x: [i32; MAX_ENTITIES],
    /// Current position Y.
    y: [i32; MAX_ENTITIES],
    /// Current position Z.
    z: [i32; MAX_ENTITIES],
    /// Facing yaw, PSX angle units.
    yaw: [i16; MAX_ENTITIES],
    /// Behavior state ([`GameEntityState`] as raw u8).
    state: [u8; MAX_ENTITIES],
    /// Ticks spent in the current state.
    state_ticks: [u16; MAX_ENTITIES],
    /// Remaining ticks for the in-place tracking-turn presentation.
    turn_ticks: [u8; MAX_ENTITIES],
    /// Phase local to the current continuous tracking turn.
    turn_phase_ticks: [u16; MAX_ENTITIES],
    /// Remaining first-channel health (Horizon).
    health: [u16; MAX_ENTITIES],
    /// Remaining second-channel health (Zenith). Each entity carries the same
    /// two vitality pools the player does; the maxima live in the cooked
    /// record, so only the two currents are per-entity state here.
    health_secondary: [u16; MAX_ENTITIES],
    /// Accumulated poise damage (staggers past the record's pool).
    poise_damage: [u16; MAX_ENTITIES],
    /// Patrol leg: 0 = toward the patrol anchor, 1 = toward spawn.
    patrol_leg: [u8; MAX_ENTITIES],
    /// Persistent eight-way local movement direction, in PSX yaw units.
    move_yaw: [u16; MAX_ENTITIES],
    /// 1 once `move_yaw` has been selected for the current behavior state.
    move_yaw_valid: [u8; MAX_ENTITIES],
    /// Directions rejected during the current bounded local search.
    move_tried: [u8; MAX_ENTITIES],
    /// Packed combat presentation/latch byte. Bit zero is one while the
    /// current Attack window already connected; bits one through four retain
    /// stance-mutation elapsed ticks; bit seven selects Zenith guard.
    combat_flags: [u8; MAX_ENTITIES],
    /// Wrapping identity of each entity's current swing. Deferred tokens use
    /// it to reject a contact retained across a later attack.
    attack_sequence: [u16; MAX_ENTITIES],
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
    /// Remaining shared delay before another attack slot may be granted.
    director_delay_ticks: u16,
    /// Wrapping identity of the latest tick/tick-delta call. Deferred contact
    /// tokens are deliberately one-call capabilities.
    attack_tick_generation: u16,
    /// Optional owner-supplied per-record activation (for BSP PVS/area
    /// residency). When disabled, the legacy active-room gate remains exact.
    spatial_activation_enabled: bool,
    // psx-numeric-allow-next-line: fixed 64-record activation mask; bit ops only, two-word on R3000
    spatial_active_mask: u64,
}

impl<const MAX_ENTITIES: usize> GameEntities<MAX_ENTITIES> {
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

    fn selected_attack_active_ticks(&self, record: &LevelGameEntityRecord, index: usize) -> u16 {
        match self.selected_attack_kind(index) {
            GAME_ENTITY_ATTACK_HEAVY => record.heavy_attack_active_ticks,
            GAME_ENTITY_ATTACK_RANGED => record.ranged_attack_active_ticks,
            _ => record.attack_active_ticks,
        }
        .max(GAME_ENTITY_ATTACK_ACTIVE_TICKS)
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

    /// Update the hybrid attack stance with hysteresis. Enter melee chase at
    /// the authored preferred distance (never inside the projectile minimum),
    /// but do not return to ranged until the player has also crossed the
    /// authored spacing tolerance.
    fn update_melee_chase(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
    ) -> bool {
        if !Self::has_ranged_attack(record) {
            return true;
        }
        let was_melee = self.attack_mode[index] & GAME_ENTITY_ATTACK_MELEE_CHASE != 0;
        let melee_entry = i32::from(record.preferred_distance.max(record.attack_min_range));
        let limit = if was_melee {
            melee_entry.saturating_add(i32::from(record.spacing_tolerance))
        } else {
            melee_entry
        };
        let melee = self.player_within(index, input, limit);
        if melee {
            self.attack_mode[index] |= GAME_ENTITY_ATTACK_MELEE_CHASE;
        } else {
            self.attack_mode[index] &= !GAME_ENTITY_ATTACK_MELEE_CHASE;
        }
        melee
    }

    /// Commit one action for the entire Windup/Attack/Recover grammar. Close
    /// attacks alternate Light, Heavy, Light, Heavy without consuming the
    /// alternation on ranged shots.
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

    fn approach_speed(record: &LevelGameEntityRecord) -> i32 {
        if Self::can_run(record) {
            record.run_speed
        } else {
            record.walk_speed
        }
    }

    /// All-zero state; `const` so the owning game can keep it in
    /// link-time-zero (`.bss`) scene storage. Not meaningful until
    /// [`Self::spawn_from_records`] runs.
    pub const EMPTY: Self = Self {
        count: 0,
        overflow: 0,
        x: [0; MAX_ENTITIES],
        y: [0; MAX_ENTITIES],
        z: [0; MAX_ENTITIES],
        yaw: [0; MAX_ENTITIES],
        state: [0; MAX_ENTITIES],
        state_ticks: [0; MAX_ENTITIES],
        turn_ticks: [0; MAX_ENTITIES],
        turn_phase_ticks: [0; MAX_ENTITIES],
        health: [0; MAX_ENTITIES],
        health_secondary: [0; MAX_ENTITIES],
        poise_damage: [0; MAX_ENTITIES],
        patrol_leg: [0; MAX_ENTITIES],
        move_yaw: [0; MAX_ENTITIES],
        move_yaw_valid: [0; MAX_ENTITIES],
        move_tried: [0; MAX_ENTITIES],
        combat_flags: [0; MAX_ENTITIES],
        attack_sequence: [0; MAX_ENTITIES],
        attack_mode: [0; MAX_ENTITIES],
        intent: [0; MAX_ENTITIES],
        attack_cooldown: [0; MAX_ENTITIES],
        attack_wait_ticks: [0; MAX_ENTITIES],
        attack_owner_plus_one: 0,
        director_delay_ticks: 0,
        attack_tick_generation: 0,
        spatial_activation_enabled: false,
        spatial_active_mask: 0,
    };

    /// Reset and spawn entity state 1:1 from the cooked records
    /// (souls checkpoint-respawn calls this again on death loops).
    /// Records past `MAX_ENTITIES` count into
    /// [`Self::overflow_count`]; records without
    /// `game_entity_flags::ENABLED` spawn [`GameEntityState::Dead`]
    /// so indices stay stable.
    pub fn spawn_from_records(&mut self, records: &'static [LevelGameEntityRecord]) {
        *self = Self::EMPTY;
        let count = records.len().min(MAX_ENTITIES);
        self.count = count as u16;
        self.overflow = (records.len() - count).min(u16::MAX as usize) as u16;
        for (index, record) in records.iter().enumerate().take(count) {
            self.x[index] = record.x;
            self.y[index] = record.y;
            self.z[index] = record.z;
            self.yaw[index] = record.yaw;
            self.health[index] = record.max_health;
            self.health_secondary[index] = record.max_health_secondary;
            self.poise_damage[index] = 0;
            self.patrol_leg[index] = 0;
            self.move_yaw[index] = 0;
            self.move_yaw_valid[index] = 0;
            self.move_tried[index] = 0;
            self.state_ticks[index] = 0;
            self.turn_ticks[index] = 0;
            self.turn_phase_ticks[index] = 0;
            self.intent[index] = GameEntityIntent::Hold as u8;
            self.attack_cooldown[index] = 0;
            self.attack_wait_ticks[index] = 0;
            self.attack_mode[index] = GAME_ENTITY_ATTACK_LIGHT;
            self.set_stance_swap_elapsed(index, GAME_ENTITY_STANCE_SWAP_DURATION_TICKS);
            let enabled = record.flags & game_entity_flags::ENABLED != 0;
            self.state[index] = if enabled {
                GameEntityState::Idle as u8
            } else {
                GameEntityState::Dead as u8
            };
        }
    }

    /// Live entity count.
    pub fn count(&self) -> usize {
        usize::from(self.count)
    }

    /// Cooked records that did not fit `MAX_ENTITIES` at spawn.
    pub fn overflow_count(&self) -> u16 {
        self.overflow
    }

    /// Select an owner-defined per-record activation mask, or restore the
    /// legacy room-window gate with `None`.
    // psx-numeric-allow-next-line: mirrors the fixed 64-record activation mask above; bit ops only
    pub fn set_spatial_active_mask(&mut self, mask: Option<u64>) {
        self.spatial_activation_enabled = mask.is_some();
        self.spatial_active_mask = mask.unwrap_or(0);
    }

    /// Behavior state of entity `index`.
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

    /// Position of entity `index`, room-local engine units.
    pub fn position(&self, index: usize) -> [i32; 3] {
        if index >= self.count() {
            return [0; 3];
        }
        [self.x[index], self.y[index], self.z[index]]
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

    /// Facing yaw of entity `index`, PSX angle units.
    pub fn yaw(&self, index: usize) -> i16 {
        if index >= self.count() {
            0
        } else {
            self.yaw[index]
        }
    }

    /// Remaining first-channel (Horizon) health of entity `index`.
    pub fn health(&self, index: usize) -> u16 {
        if index >= self.count() {
            0
        } else {
            self.health[index]
        }
    }

    /// Remaining second-channel (Zenith) health of entity `index`.
    pub fn health_secondary(&self, index: usize) -> u16 {
        if index >= self.count() {
            0
        } else {
            self.health_secondary[index]
        }
    }

    /// Remaining health in one named vitality channel of entity `index`.
    pub fn health_channel(&self, index: usize, channel: VitalityChannelId) -> u16 {
        match channel {
            VitalityChannelId::One => self.health(index),
            VitalityChannelId::Two => self.health_secondary(index),
        }
    }

    /// Vitality channel currently guarded by entity `index`.
    /// Out-of-range entities fail to Horizon, the all-zero packed default.
    pub fn stance(&self, index: usize) -> VitalityChannelId {
        if index < self.count() && self.combat_flags[index] & GAME_ENTITY_STANCE_ZENITH != 0 {
            VitalityChannelId::Two
        } else {
            VitalityChannelId::One
        }
    }

    /// Q12 presentation progress for the entity's current guard mutation.
    /// Settled and invalid entities return one, matching player HUD semantics.
    pub fn stance_swap_progress_q12(&self, index: usize) -> u16 {
        if index >= self.count() {
            return 4096;
        }
        let elapsed = self.stance_swap_elapsed(index);
        ((u32::from(elapsed) * 4096) / u32::from(GAME_ENTITY_STANCE_SWAP_DURATION_TICKS)).min(4096)
            as u16
    }

    /// Whether the entity's rising stance-colour tell is still active.
    pub fn stance_swap_in_progress(&self, index: usize) -> bool {
        index < self.count()
            && self.stance_swap_elapsed(index) < GAME_ENTITY_STANCE_SWAP_DURATION_TICKS
    }

    /// Defensive damage scale selected by the target's current guard.
    pub fn stance_damage_scale_q12(&self, index: usize, attack: VitalityChannelId) -> u16 {
        if attack == self.stance(index) {
            GAME_ENTITY_GUARDED_DAMAGE_Q12
        } else {
            GAME_ENTITY_OPPOSED_DAMAGE_Q12
        }
    }

    /// Apply the current enemy guard multiplier to one authored damage value.
    pub fn scaled_stance_damage(
        &self,
        index: usize,
        attack: VitalityChannelId,
        damage: u16,
    ) -> u16 {
        let scale = self.stance_damage_scale_q12(index, attack);
        ((u32::from(damage) * u32::from(scale)) / 4096).min(u32::from(u16::MAX)) as u16
    }

    fn stance_swap_elapsed(&self, index: usize) -> u8 {
        (self.combat_flags[index] & GAME_ENTITY_STANCE_SWAP_MASK) >> GAME_ENTITY_STANCE_SWAP_SHIFT
    }

    fn set_stance_swap_elapsed(&mut self, index: usize, elapsed: u8) {
        let elapsed = elapsed.min(GAME_ENTITY_STANCE_SWAP_DURATION_TICKS);
        self.combat_flags[index] = (self.combat_flags[index] & !GAME_ENTITY_STANCE_SWAP_MASK)
            | (elapsed << GAME_ENTITY_STANCE_SWAP_SHIFT);
    }

    fn advance_stance_swap(&mut self, index: usize, delta_ticks: u16) {
        let elapsed = self
            .stance_swap_elapsed(index)
            .saturating_add(delta_ticks.min(u16::from(u8::MAX)) as u8);
        self.set_stance_swap_elapsed(index, elapsed);
    }

    fn mutate_stance(&mut self, index: usize) {
        self.combat_flags[index] ^= GAME_ENTITY_STANCE_ZENITH;
        self.set_stance_swap_elapsed(index, 0);
    }

    /// Entity `index`'s two vitality pools, rehydrated from the dense state
    /// tables and the cooked maxima. The whole point is that enemy vitality
    /// obeys [`DualVitality`] itself rather than a parallel reimplementation.
    fn vitality(&self, records: &[LevelGameEntityRecord], index: usize) -> DualVitality {
        let record = &records[index];
        DualVitality::from_pools(
            VitalityPool::at(self.health[index], record.max_health),
            VitalityPool::at(self.health_secondary[index], record.max_health_secondary),
        )
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
        if index >= self.count() || index >= records.len() {
            return GameEntityClip::default();
        }
        let record = &records[index];
        let ticks = self.state_ticks[index];
        let attack_clip = self.selected_attack_clip(record, index);
        let attack_active_ticks = self.selected_attack_active_ticks(record, index);
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
        let attack_one_shot = |phase_ticks: u16| GameEntityClip {
            clip: attack_clip,
            phase_ticks,
            one_shot: true,
            speed_q8: attack_speed_q8,
            frame_range: attack_frame_range,
        };
        match self.state(index) {
            GameEntityState::Idle => looping(record.idle_clip),
            GameEntityState::Patrol => looping(record.walk_clip),
            GameEntityState::Aggro if ticks < u16::from(record.reaction_ticks) => {
                one_shot(record.alert_clip, ticks)
            }
            GameEntityState::Aggro => match self.intent(index) {
                GameEntityIntent::Approach => looping(Self::approach_clip(record)),
                GameEntityIntent::CircleLeft => looping(record.strafe_left_clip),
                GameEntityIntent::CircleRight => looping(record.strafe_right_clip),
                GameEntityIntent::Retreat => looping(record.walk_backward_clip),
                GameEntityIntent::Hold if self.turn_ticks[index] != 0 => GameEntityClip {
                    clip: record.turn_clip,
                    phase_ticks: self.turn_phase_ticks[index],
                    one_shot: false,
                    speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
                    frame_range: CharacterActionFrameRange::FULL,
                },
                GameEntityIntent::Hold => looping(record.idle_clip),
            },
            GameEntityState::Windup => attack_one_shot(ticks),
            GameEntityState::Attack => {
                attack_one_shot(u16::from(record.windup_ticks).saturating_add(ticks))
            }
            GameEntityState::Recover => attack_one_shot(
                u16::from(record.windup_ticks)
                    .saturating_add(attack_active_ticks)
                    .saturating_add(ticks),
            ),
            GameEntityState::Staggered => one_shot(record.stagger_clip, ticks),
            GameEntityState::Dead => one_shot(record.death_clip, ticks),
        }
    }

    /// Apply a hit to entity `index`: channel-routed health damage plus poise
    /// damage. Accumulated poise damage past the record's pool staggers (and
    /// the accumulator resets). This is the raw, unscaled path retained for
    /// callers that do not participate in Cortex stance combat; authored
    /// player attacks use [`Self::apply_stance_hit`].
    ///
    /// `channel` is the attack's vitality channel, which the player side
    /// already derives from the swing (horizontal = Horizon, vertical =
    /// Zenith). Routing mirrors the player's own untyped path exactly:
    /// [`DualVitality::apply_spill`] consumes the named channel first and
    /// spills only the excess into the other, so a single-channel attacker
    /// still kills at the same total damage while each half of the gauge
    /// drains on the attacks that own it. The entity dies when BOTH pools are
    /// empty, which is [`DualVitality::is_defeated`] and nothing local.
    pub fn apply_hit(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        channel: VitalityChannelId,
        damage: u16,
        poise_damage: u16,
    ) -> GameEntityHitOutcome {
        self.apply_scaled_hit(records, index, channel, damage, poise_damage)
    }

    /// Apply a Cortex-style stance-aware hit. The guarded channel takes 50%
    /// damage and the exposed channel takes 150%; routing and spill remain the
    /// same as [`Self::apply_hit`]. Poise stays authored and unscaled.
    pub fn apply_stance_hit(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        channel: VitalityChannelId,
        damage: u16,
        poise_damage: u16,
    ) -> GameEntityHitOutcome {
        let damage = self.scaled_stance_damage(index, channel, damage);
        self.apply_scaled_hit(records, index, channel, damage, poise_damage)
    }

    fn apply_scaled_hit(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        channel: VitalityChannelId,
        damage: u16,
        poise_damage: u16,
    ) -> GameEntityHitOutcome {
        if index >= self.count() || index >= records.len() {
            return GameEntityHitOutcome::MISS;
        }
        if self.state(index) == GameEntityState::Dead {
            return GameEntityHitOutcome::MISS;
        }
        let mut vitality = self.vitality(records, index);
        let defeated = vitality.apply_spill(channel, damage).actor_defeated;
        self.health[index] = vitality.pool(VitalityChannelId::One).current();
        self.health_secondary[index] = vitality.pool(VitalityChannelId::Two).current();
        if defeated {
            self.release_attack_owner(index, u16::from(records[index].group_attack_delay_ticks));
            self.enter_state(
                index,
                GameEntityState::Dead,
                &mut GameEntityTickStats::default(),
            );
            return GameEntityHitOutcome {
                connected: true,
                staggered: false,
                died: true,
            };
        }
        self.poise_damage[index] = self.poise_damage[index].saturating_add(poise_damage);
        let staggered = self.poise_damage[index] > records[index].poise;
        if staggered {
            self.poise_damage[index] = 0;
            self.release_attack_owner(index, u16::from(records[index].group_attack_delay_ticks));
            self.enter_state(
                index,
                GameEntityState::Staggered,
                &mut GameEntityTickStats::default(),
            );
        }
        GameEntityHitOutcome {
            connected: true,
            staggered,
            died: false,
        }
    }

    /// Sweep the player's melee arc over the live entities: every
    /// enemy in the arc's room whose hurtbox cylinder the arc reaches
    /// (and whose bit in `already_hit` is clear) takes one
    /// [`Self::apply_stance_hit`], and its bit latches so one swing connects
    /// at most once per enemy. `O(live entities)` with the
    /// per-axis/squared early-outs of [`arc_hits_circle`]; the owning
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
            already_hit,
            |_, _| false,
        )
    }

    /// [`Self::apply_melee_arc`] with a caller-supplied world occlusion test.
    /// `occluded(entity, position)` returning true blocks the connection
    /// WITHOUT latching the swing bit, so a target revealed later in the same
    /// active window (a door finishing its travel) can still be hit once.
    pub fn apply_melee_arc_occluded(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        arc: &MeleeArc,
        channel: VitalityChannelId,
        damage: u16,
        poise_damage: u16,
        // psx-numeric-allow-next-line: one-hit-per-swing bitmask; bit ops only, two-word on R3000
        already_hit: &mut u64,
        mut occluded: impl FnMut(usize, [i32; 3]) -> bool,
    ) -> MeleeArcStats {
        // psx-numeric-allow-next-line: the 64-record cap IS the mask width
        const { assert!(MAX_ENTITIES <= 64, "swing mask is a u64") };
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
                || !arc_hits_circle(
                    arc,
                    self.x[entity],
                    self.z[entity],
                    i32::from(record.radius),
                )
                || occluded(entity, self.position(entity));
            if !skip {
                *already_hit |= mask;
                let outcome = self.apply_stance_hit(records, entity, channel, damage, poise_damage);
                stats.hits += u16::from(outcome.connected);
                stats.staggers += u16::from(outcome.staggered);
                stats.deaths += u16::from(outcome.died);
            }
            entity += 1;
        }
        stats
    }

    /// Advance every entity one 60 Hz tick. Thinking is gated on the
    /// active-room set hl-psx-style: an entity outside the set only
    /// thinks while its behavior is engaged (anything past
    /// Idle/Patrol), and an entity whose room id is out of range
    /// stays awake -- a cook failure can never freeze an actor.
    pub fn tick(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
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
        mover: &mut impl GameEntityMover,
        delta_ticks: u16,
    ) -> GameEntityTickStats {
        self.tick_delta_impl::<0>(records, input, mover, delta_ticks, None)
    }

    /// Advance entity behaviour while deferring attack contact to the owner of
    /// the retained actor poses.
    ///
    /// Every active attack writes one [`DeferredGameEntityAttack`] containing
    /// the exact attack clip/phase and transform sampled before any
    /// Attack-to-Recover transition. The caller resolves body/equipment poses,
    /// evaluates authored combat capsules, and latches a connection with
    /// [`Self::connect_deferred_attack`]. `player_hits` and `player_damage` in
    /// the returned stats are therefore zero; the caller reports those after
    /// pose-backed contact resolution.
    pub fn tick_delta_deferred<const MAX_ATTACKS: usize>(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
        delta_ticks: u16,
        attacks: &mut DeferredGameEntityAttacks<MAX_ATTACKS>,
    ) -> GameEntityTickStats {
        attacks.clear();
        self.tick_delta_impl(records, input, mover, delta_ticks, Some(attacks))
    }

    fn tick_delta_impl<const MAX_ATTACKS: usize>(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
        delta_ticks: u16,
        mut deferred: Option<&mut DeferredGameEntityAttacks<MAX_ATTACKS>>,
    ) -> GameEntityTickStats {
        let delta_ticks = delta_ticks.max(1);
        self.attack_tick_generation = self.attack_tick_generation.wrapping_add(1);
        let mut stats = GameEntityTickStats::default();
        let count = self.count().min(records.len());
        self.update_combat_director(records, input, delta_ticks, &mut stats);
        let mut index = 0usize;
        while index < count {
            let record = &records[index];
            let state = GameEntityState::from_raw(self.state[index]);
            if state == GameEntityState::Dead {
                // Dead entities stop thinking but keep counting so
                // the death one-shot plays out and then holds its
                // final frame (see clip_for_state).
                self.state_ticks[index] = self.state_ticks[index].saturating_add(delta_ticks);
                index += 1;
                continue;
            }
            self.advance_stance_swap(index, delta_ticks);
            let behavior_awake = !matches!(state, GameEntityState::Idle | GameEntityState::Patrol);
            let spatially_active = index < 64 && self.spatial_active_mask & (1u64 << index) != 0;
            let activation_allows = if self.spatial_activation_enabled {
                spatially_active
            } else {
                room_is_active(record.room, input.active_rooms)
            };
            if !behavior_awake && !activation_allows {
                stats.gated += 1;
                index += 1;
                continue;
            }
            stats.thought += 1;
            if self.turn_ticks[index] == 0 {
                self.turn_phase_ticks[index] = 0;
            } else {
                self.turn_ticks[index] = self.turn_ticks[index]
                    .saturating_sub(delta_ticks.min(u16::from(u8::MAX)) as u8);
                self.turn_phase_ticks[index] =
                    self.turn_phase_ticks[index].saturating_add(delta_ticks);
            }
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
                    if self.state_ticks[index] >= u16::from(record.windup_ticks) {
                        self.enter_state(index, GameEntityState::Attack, &mut stats);
                    }
                }
                GameEntityState::Attack => {
                    stats.attacking += 1;
                    match deferred.as_deref_mut() {
                        Some(attacks) => attacks.push(self.deferred_attack(record, index)),
                        None => self.resolve_attack_contact(record, index, input, &mut stats),
                    }
                    if self.state_ticks[index] >= self.selected_attack_active_ticks(record, index) {
                        self.enter_state(index, GameEntityState::Recover, &mut stats);
                    }
                }
                GameEntityState::Recover => {
                    if self.state_ticks[index] >= u16::from(record.recovery_ticks) {
                        self.attack_cooldown[index] = u16::from(record.attack_cooldown_ticks);
                        self.release_attack_owner(
                            index,
                            u16::from(record.group_attack_delay_ticks),
                        );
                        self.enter_state(index, GameEntityState::Aggro, &mut stats);
                    }
                }
                GameEntityState::Staggered => {
                    if self.state_ticks[index] >= GAME_ENTITY_STAGGER_TICKS {
                        self.enter_state(index, GameEntityState::Aggro, &mut stats);
                        // The authored reaction is for first acquisition, not
                        // an extra pause after the player already won a stagger.
                        self.state_ticks[index] = u16::from(record.reaction_ticks);
                    }
                }
                GameEntityState::Dead => {}
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
                phase_ticks: u16::from(record.windup_ticks).saturating_add(self.state_ticks[index]),
                one_shot: true,
                speed_q8,
                frame_range,
            },
            action,
            ranged,
            position: [self.x[index], self.y[index], self.z[index]],
            yaw: self.yaw[index] as u16,
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

    /// Test the explicitly legacy Character-radius/front-arc contact policy for
    /// a deferred token. Games should call this only when authored attacker
    /// hitboxes or defender hurtboxes are absent; an authored inactive frame or
    /// authored geometric miss is authoritative and must not fall back here.
    pub fn deferred_attack_legacy_arc_hits(
        &self,
        records: &[LevelGameEntityRecord],
        attack: DeferredGameEntityAttack,
        player: [i32; 3],
        player_room: RoomIndex,
        player_radius: i32,
    ) -> bool {
        let index = attack.entity();
        if !self.deferred_attack_can_connect(attack)
            || records.get(index).is_none()
            || player_room != attack.room
        {
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
            reach: i32::from(record.radius)
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
                && record.room == input.player_room
                && self.attack_cooldown[index] == 0
                && self.state_ticks[index] >= u16::from(record.reaction_ticks)
                && self.player_within(
                    index,
                    input,
                    i32::from(record.preferred_distance)
                        .max(Self::attack_reach(record, input))
                        .saturating_add(i32::from(record.spacing_tolerance)),
                );
            if ready {
                let dx = self.x[index].saturating_sub(input.player[0]).abs();
                let dz = self.z[index].saturating_sub(input.player[2]).abs();
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
            self.attack_wait_ticks[index] = 0;
            self.set_intent(index, GameEntityIntent::Approach);
            stats.attack_grants = stats.attack_grants.saturating_add(1);
        }
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
        self.state[index] = state as u8;
        self.state_ticks[index] = 0;
        self.move_yaw_valid[index] = 0;
        self.move_tried[index] = 0;
        if state != GameEntityState::Aggro {
            self.set_intent(index, GameEntityIntent::Hold);
            self.turn_ticks[index] = 0;
            self.turn_phase_ticks[index] = 0;
        }
        if matches!(state, GameEntityState::Idle | GameEntityState::Dead) {
            self.attack_wait_ticks[index] = 0;
        }
        match state {
            GameEntityState::Patrol => stats.patrol_enters += 1,
            GameEntityState::Aggro => stats.aggro_enters += 1,
            GameEntityState::Windup => stats.windup_enters += 1,
            GameEntityState::Attack => {
                stats.attack_enters += 1;
                // A fresh swing gets one connection.
                self.combat_flags[index] &= !GAME_ENTITY_ATTACK_CONNECTED;
                self.attack_sequence[index] = self.attack_sequence[index].wrapping_add(1);
            }
            // Recover is the player's punish window, so rotate the guard here:
            // the twelve-tick colour sweep reads before another attack grant.
            GameEntityState::Recover => self.mutate_stance(index),
            _ => {}
        }
    }

    /// One Attack-window contact test against the player: the same
    /// Character-derived reach that committed the windup, gated to a
    /// front arc around the facing the entity locked when it
    /// committed. Connects at most once per swing, and never against
    /// an i-framing player (see [`GameEntityTickInput::player_invulnerable`]).
    fn resolve_attack_contact(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        stats: &mut GameEntityTickStats,
    ) {
        if self.selected_attack_is_ranged(index)
            || self.combat_flags[index] & GAME_ENTITY_ATTACK_CONNECTED != 0
            || input.player_invulnerable
            || input.player_room != record.room
        {
            return;
        }
        let arc = MeleeArc {
            room: record.room,
            x: self.x[index],
            z: self.z[index],
            yaw: self.yaw[index] as u16,
            reach: Self::melee_attack_reach(record, input),
            half_angle: GAME_ENTITY_ATTACK_HALF_ANGLE,
        };
        // The player hurtbox center is the motor position; its radius
        // is already inside `attack_reach` (radius + radius + margin),
        // so the arc tests the CENTER (radius 0) to avoid counting the
        // player capsule twice.
        if !arc_hits_circle(&arc, input.player[0], input.player[2], 0) {
            return;
        }
        self.combat_flags[index] |= GAME_ENTITY_ATTACK_CONNECTED;
        stats.player_hits += 1;
        stats.player_damage = stats.player_damage.saturating_add(record.touch_damage);
    }

    fn player_within(&self, index: usize, input: GameEntityTickInput<'_>, radius: i32) -> bool {
        within_xz(
            [self.x[index], self.z[index]],
            [input.player[0], input.player[2]],
            radius,
        )
    }

    /// Attack-band outer edge. Ranged attacks use their authored maximum;
    /// melee uses both body radii plus the close-in margin.
    fn attack_reach(record: &LevelGameEntityRecord, input: GameEntityTickInput<'_>) -> i32 {
        if Self::has_ranged_attack(record) {
            i32::from(record.attack_max_range)
        } else {
            i32::from(record.radius)
                .saturating_add(input.player_radius.max(0))
                .saturating_add(GAME_ENTITY_ATTACK_REACH_MARGIN)
        }
    }

    fn melee_attack_reach(record: &LevelGameEntityRecord, input: GameEntityTickInput<'_>) -> i32 {
        i32::from(record.radius)
            .saturating_add(input.player_radius.max(0))
            .saturating_add(GAME_ENTITY_ATTACK_REACH_MARGIN)
    }

    /// Aggro notice test: distance AND same room. Cooked positions
    /// are room-local, so a raw distance compare aliases across
    /// rooms; cross-room notice (portal line-of-sight) is the nav
    /// slice's work.
    fn player_noticed(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
    ) -> bool {
        if input.player_room != record.room
            || !self.player_within(index, input, i32::from(record.aggro_radius))
        {
            return false;
        }
        self.player_in_line_of_sight(record, index, input, mover)
    }

    fn player_in_line_of_sight(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
    ) -> bool {
        let from = [
            self.x[index],
            self.y[index].saturating_add(i32::from(record.height) / 2),
            self.z[index],
        ];
        let to = [
            input.player[0],
            input.player[1].saturating_add(input.player_height.max(1) / 2),
            input.player[2],
        ];
        mover.line_of_sight(record.room, from, to)
    }

    fn tick_idle(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
        stats: &mut GameEntityTickStats,
    ) {
        if self.player_noticed(record, index, input, mover) {
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
        mover: &mut impl GameEntityMover,
        delta_ticks: u16,
        stats: &mut GameEntityTickStats,
    ) {
        if self.player_noticed(record, index, input, mover) {
            self.enter_state(index, GameEntityState::Aggro, stats);
            return;
        }
        let goal = if self.patrol_leg[index] == 0 {
            [record.patrol_x, record.patrol_y, record.patrol_z]
        } else {
            [record.x, record.y, record.z]
        };
        let speed = record.walk_speed.saturating_mul(i32::from(delta_ticks));
        if self.step_toward(record, index, goal, speed, mover) {
            self.patrol_leg[index] ^= 1;
            self.enter_state(index, GameEntityState::Idle, stats);
        }
    }

    fn tick_aggro(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
        delta_ticks: u16,
        stats: &mut GameEntityTickStats,
    ) {
        let leash = i32::from(record.aggro_radius).saturating_mul(GAME_ENTITY_LEASH_FACTOR);
        if input.player_room != record.room || !self.player_within(index, input, leash) {
            // Souls de-aggro: drop the chase and return to the idle
            // loop (return-to-post pathing is the nav slice).
            self.release_attack_owner(index, u16::from(record.group_attack_delay_ticks));
            self.enter_state(index, GameEntityState::Idle, stats);
            return;
        }

        if self.state_ticks[index] < u16::from(record.reaction_ticks) {
            self.set_intent(index, GameEntityIntent::Hold);
            self.face_toward(index, input.player);
            // The acquisition one-shot owns this window. A facing snap here
            // must not leak a stale turn phase into the first combat hold.
            self.turn_ticks[index] = 0;
            self.turn_phase_ticks[index] = 0;
            stats.holding = stats.holding.saturating_add(1);
            return;
        }

        // Stance selection is independent of the shared attack token. An
        // enemy that has entered close combat must continue following the
        // player while another actor owns the swing, rather than falling back
        // to the ranged standoff ring until its turn arrives.
        let melee_chase = self.update_melee_chase(record, index, input);

        if self.attack_owner() == Some(index) {
            self.set_intent(index, GameEntityIntent::Approach);
            let commit_reach = if melee_chase {
                Self::melee_attack_reach(record, input)
            } else {
                Self::attack_reach(record, input)
            };
            if self.player_within(index, input, commit_reach)
                && self.player_in_line_of_sight(record, index, input, mover)
            {
                self.face_toward(index, input.player);
                self.select_attack(index, !melee_chase);
                self.enter_state(index, GameEntityState::Windup, stats);
                return;
            }
            self.step_toward(
                record,
                index,
                input.player,
                Self::approach_speed(record).saturating_mul(i32::from(delta_ticks)),
                mover,
            );
            return;
        }

        if melee_chase && Self::has_ranged_attack(record) {
            let melee_reach = Self::melee_attack_reach(record, input);
            if !self.player_within(index, input, melee_reach) {
                self.set_intent(index, GameEntityIntent::Approach);
                self.step_toward(
                    record,
                    index,
                    input.player,
                    Self::approach_speed(record).saturating_mul(i32::from(delta_ticks)),
                    mover,
                );
            } else {
                self.set_intent(index, GameEntityIntent::Hold);
                self.face_toward(index, input.player);
                stats.holding = stats.holding.saturating_add(1);
            }
            return;
        }

        let attack_reach = Self::attack_reach(record, input);
        let preferred = i32::from(record.preferred_distance).max(attack_reach);
        let tolerance = i32::from(record.spacing_tolerance).min(preferred);
        let near_edge = preferred.saturating_sub(tolerance);
        let far_edge = preferred.saturating_add(tolerance);
        if !self.player_within(index, input, far_edge) {
            self.set_intent(index, GameEntityIntent::Approach);
            self.step_toward(
                record,
                index,
                input.player,
                Self::approach_speed(record).saturating_mul(i32::from(delta_ticks)),
                mover,
            );
            return;
        }
        if self.player_within(index, input, near_edge) {
            self.set_intent(index, GameEntityIntent::Retreat);
            self.step_relative_to_player(
                record,
                index,
                input,
                GAME_ENTITY_HALF_TURN,
                record.walk_speed.saturating_mul(i32::from(delta_ticks)),
                mover,
            );
            stats.retreating = stats.retreating.saturating_add(1);
            return;
        }

        let interval = u16::from(record.decision_interval_ticks).max(1);
        let epoch = self.state_ticks[index] / interval;
        let choice = (u32::from(epoch) * 37 + index as u32 * 17) % 100;
        if choice < u32::from(record.circle_chance.min(100)) {
            let left = (u32::from(epoch) + index as u32).is_multiple_of(2);
            let intent = if left {
                GameEntityIntent::CircleLeft
            } else {
                GameEntityIntent::CircleRight
            };
            self.set_intent(index, intent);
            let yaw_offset = if left {
                GAME_ENTITY_QUARTER_TURN.wrapping_neg()
            } else {
                GAME_ENTITY_QUARTER_TURN
            };
            self.step_relative_to_player(
                record,
                index,
                input,
                yaw_offset,
                record.walk_speed.saturating_mul(i32::from(delta_ticks)),
                mover,
            );
            stats.circling = stats.circling.saturating_add(1);
        } else {
            self.set_intent(index, GameEntityIntent::Hold);
            self.face_toward(index, input.player);
            stats.holding = stats.holding.saturating_add(1);
        }
    }

    /// Move at a yaw offset from the direction to the player, then restore
    /// player-facing. This gives circle/retreat movement without requiring a
    /// navigation allocation or a second target point.
    fn step_relative_to_player(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        yaw_offset: u16,
        speed: i32,
        mover: &mut impl GameEntityMover,
    ) {
        let dx = input.player[0].saturating_sub(self.x[index]);
        let dz = input.player[2].saturating_sub(self.z[index]);
        if dx == 0 && dz == 0 {
            return;
        }
        let move_yaw = atan2_q12(dx, dz).wrapping_add(yaw_offset) & 0x0fff;
        let sin = psx_math::sin_q12(move_yaw);
        let cos = psx_math::cos_q12(move_yaw);
        let speed = speed.max(1);
        let step_x = (sin * speed) >> 12;
        let step_z = (cos * speed) >> 12;
        let position = [self.x[index], self.y[index], self.z[index]];
        let committed = mover.step(
            index,
            record.room,
            position,
            step_x,
            step_z,
            i32::from(record.radius),
            i32::from(record.height).max(1),
        );
        self.x[index] = committed[0];
        self.y[index] = committed[1];
        self.z[index] = committed[2];
        self.face_toward(index, input.player);
    }

    /// One motor-checked step toward `goal` in XZ at `speed` engine
    /// units per tick. Free movement persists one of eight Quake-style
    /// chase directions. A blocked direction invokes a bounded local
    /// search over the remaining directions, with the turnaround tried
    /// last. Returns `true` on arrival (within one step of the goal and
    /// the final hop committed).
    fn step_toward(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        goal: [i32; 3],
        speed: i32,
        mover: &mut impl GameEntityMover,
    ) -> bool {
        let speed = speed.max(1);
        let dx = goal[0].saturating_sub(self.x[index]);
        let dz = goal[2].saturating_sub(self.z[index]);
        if dx == 0 && dz == 0 {
            return true;
        }
        let arriving = dx.abs() <= speed && dz.abs() <= speed;
        if arriving {
            let position = [self.x[index], self.y[index], self.z[index]];
            if self.try_exact_step(record, index, dx, dz, mover) {
                self.move_yaw_valid[index] = 0;
                self.move_tried[index] = 0;
                return true;
            }
            if self.x[index] != position[0] || self.z[index] != position[2] {
                return false;
            }
        }

        // Quake reconsiders one movement call in four even when its current
        // direction still works. Derive the choice from entity-local state so
        // host tests, replays, and console runs make identical decisions.
        let choice = self.state_ticks[index]
            .wrapping_mul(37)
            .wrapping_add((index as u16).wrapping_mul(17));
        let reconsider = self.move_yaw_valid[index] == 0 || choice & 3 == 1;
        let mut tried = if reconsider {
            0
        } else {
            self.move_tried[index]
        };
        let mut probes = 0u8;
        if !reconsider {
            let old_yaw = self.move_yaw[index];
            let old_bit = Self::direction_bit(old_yaw);
            if tried & old_bit == 0 {
                tried |= old_bit;
                probes += 1;
                if self.step_direction(record, index, old_yaw, speed, mover) {
                    self.move_tried[index] = 0;
                    return false;
                }
            }
        }

        self.new_chase_direction(record, index, goal, speed, choice, tried, probes, mover);
        false
    }

    /// Try one exact final hop. Arrival should not be forced through the
    /// eight-way steering grid or it can orbit a goal forever at low speeds.
    fn try_exact_step(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        dx: i32,
        dz: i32,
        mover: &mut impl GameEntityMover,
    ) -> bool {
        let position = [self.x[index], self.y[index], self.z[index]];
        let committed = mover.step(
            index,
            record.room,
            position,
            dx,
            dz,
            i32::from(record.radius),
            i32::from(record.height).max(1),
        );
        self.commit_step(index, position, committed);
        committed[0] == position[0].saturating_add(dx)
            && committed[2] == position[2].saturating_add(dz)
    }

    /// Quake's `newchasedir` ordering, adapted to deterministic entity-local
    /// entropy and an eight-bit attempted-direction mask. The mask removes the
    /// duplicate probes present in the original C routine while preserving its
    /// preference order. Two probes run now; the remaining directions resume
    /// on later NPC ticks from the retained mask.
    #[allow(clippy::too_many_arguments)]
    fn new_chase_direction(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        goal: [i32; 3],
        speed: i32,
        choice: u16,
        mut tried: u8,
        mut probes: u8,
        mover: &mut impl GameEntityMover,
    ) {
        let dx = goal[0].saturating_sub(self.x[index]);
        let dz = goal[2].saturating_sub(self.z[index]);
        let old_yaw = if self.move_yaw_valid[index] != 0 {
            self.move_yaw[index] & GAME_ENTITY_YAW_MASK
        } else {
            Self::nearest_direction(atan2_q12(dx, dz))
        };
        let turnaround = old_yaw.wrapping_add(GAME_ENTITY_HALF_TURN) & GAME_ENTITY_YAW_MASK;

        let x_yaw = if dx > GAME_ENTITY_CHASE_AXIS_EPSILON {
            Some(GAME_ENTITY_QUARTER_TURN)
        } else if dx < -GAME_ENTITY_CHASE_AXIS_EPSILON {
            Some(GAME_ENTITY_QUARTER_TURN.wrapping_mul(3))
        } else {
            None
        };
        let z_yaw = if dz > GAME_ENTITY_CHASE_AXIS_EPSILON {
            Some(0)
        } else if dz < -GAME_ENTITY_CHASE_AXIS_EPSILON {
            Some(GAME_ENTITY_HALF_TURN)
        } else {
            None
        };

        if let (Some(x_yaw), Some(z_yaw)) = (x_yaw, z_yaw) {
            let diagonal = match (x_yaw, z_yaw) {
                (GAME_ENTITY_QUARTER_TURN, 0) => GAME_ENTITY_DIRECTION_STEP,
                (GAME_ENTITY_QUARTER_TURN, GAME_ENTITY_HALF_TURN) => {
                    GAME_ENTITY_QUARTER_TURN + GAME_ENTITY_DIRECTION_STEP
                }
                (yaw, GAME_ENTITY_HALF_TURN) if yaw == GAME_ENTITY_QUARTER_TURN.wrapping_mul(3) => {
                    GAME_ENTITY_HALF_TURN + GAME_ENTITY_DIRECTION_STEP
                }
                _ => GAME_ENTITY_HALF_TURN + GAME_ENTITY_QUARTER_TURN + GAME_ENTITY_DIRECTION_STEP,
            };
            if diagonal != turnaround
                && self.try_chase_direction(
                    record,
                    index,
                    diagonal,
                    speed,
                    &mut tried,
                    &mut probes,
                    mover,
                )
            {
                return;
            }
        }

        let mut first_axis = x_yaw;
        let mut second_axis = z_yaw;
        if choice & 3 != 0 || dz.saturating_abs() > dx.saturating_abs() {
            core::mem::swap(&mut first_axis, &mut second_axis);
        }
        for yaw in [first_axis, second_axis].into_iter().flatten() {
            if yaw != turnaround
                && self.try_chase_direction(
                    record,
                    index,
                    yaw,
                    speed,
                    &mut tried,
                    &mut probes,
                    mover,
                )
            {
                return;
            }
        }

        if old_yaw != turnaround
            && self.try_chase_direction(
                record,
                index,
                old_yaw,
                speed,
                &mut tried,
                &mut probes,
                mover,
            )
        {
            return;
        }

        if choice & 4 != 0 {
            let mut direction = 0u16;
            while direction < 4096 {
                if direction != turnaround
                    && self.try_chase_direction(
                        record,
                        index,
                        direction,
                        speed,
                        &mut tried,
                        &mut probes,
                        mover,
                    )
                {
                    return;
                }
                direction += GAME_ENTITY_DIRECTION_STEP;
            }
        } else {
            let mut direction = 4096u16;
            while direction != 0 {
                direction -= GAME_ENTITY_DIRECTION_STEP;
                if direction != turnaround
                    && self.try_chase_direction(
                        record,
                        index,
                        direction,
                        speed,
                        &mut tried,
                        &mut probes,
                        mover,
                    )
                {
                    return;
                }
            }
        }

        if self.try_chase_direction(
            record,
            index,
            turnaround,
            speed,
            &mut tried,
            &mut probes,
            mover,
        ) {
            return;
        }
        self.move_yaw[index] = old_yaw;
        self.move_yaw_valid[index] = 1;
        self.move_tried[index] = if tried == u8::MAX { 0 } else { tried };
    }

    #[allow(clippy::too_many_arguments)]
    fn try_chase_direction(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        yaw: u16,
        speed: i32,
        tried: &mut u8,
        probes: &mut u8,
        mover: &mut impl GameEntityMover,
    ) -> bool {
        let bit = Self::direction_bit(yaw);
        if *tried & bit != 0 || *probes >= GAME_ENTITY_DIRECTION_PROBES_PER_TICK {
            return false;
        }
        *tried |= bit;
        *probes += 1;
        if !self.step_direction(record, index, yaw, speed, mover) {
            return false;
        }
        self.move_yaw[index] = yaw & GAME_ENTITY_YAW_MASK;
        self.move_yaw_valid[index] = 1;
        self.move_tried[index] = 0;
        true
    }

    /// Attempt one quantized direction through the existing collision motor.
    fn step_direction(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        yaw: u16,
        speed: i32,
        mover: &mut impl GameEntityMover,
    ) -> bool {
        let sin = psx_math::sin_q12(yaw);
        let cos = psx_math::cos_q12(yaw);
        let step_x = Self::q12_step_component(sin, speed);
        let step_z = Self::q12_step_component(cos, speed);
        let position = [self.x[index], self.y[index], self.z[index]];
        let committed = mover.step_direction(
            index,
            record.room,
            position,
            step_x,
            step_z,
            i32::from(record.radius),
            i32::from(record.height).max(1),
        );
        let moved = committed[0] != position[0] || committed[2] != position[2];
        self.commit_step(index, position, committed);
        moved
    }

    fn commit_step(&mut self, index: usize, position: [i32; 3], committed: [i32; 3]) {
        self.x[index] = committed[0];
        self.y[index] = committed[1];
        self.z[index] = committed[2];
        let dx = committed[0].saturating_sub(position[0]);
        let dz = committed[2].saturating_sub(position[2]);
        if dx != 0 || dz != 0 {
            self.yaw[index] = atan2_q12(dx, dz) as i16;
        }
    }

    /// Preserve a non-zero component for one-unit low-detail speeds. Quake's
    /// fixed-point positions retain that fraction; the runtime's integer room
    /// coordinates need an explicit one-unit step instead.
    fn q12_step_component(direction_q12: i32, speed: i32) -> i32 {
        let product = direction_q12.saturating_mul(speed.max(1));
        let component = product >> 12;
        if component == 0 && product != 0 {
            product.signum()
        } else {
            component
        }
    }

    fn nearest_direction(yaw: u16) -> u16 {
        ((yaw.wrapping_add(GAME_ENTITY_DIRECTION_STEP / 2) & GAME_ENTITY_YAW_MASK)
            / GAME_ENTITY_DIRECTION_STEP)
            * GAME_ENTITY_DIRECTION_STEP
    }

    fn direction_bit(yaw: u16) -> u8 {
        1u8 << ((yaw & GAME_ENTITY_YAW_MASK) / GAME_ENTITY_DIRECTION_STEP)
    }

    /// Face the XZ direction toward `goal` (PSX angle units, the
    /// motor's yaw convention: x = sin, z = cos).
    fn face_toward(&mut self, index: usize, goal: [i32; 3]) {
        let dx = goal[0].saturating_sub(self.x[index]);
        let dz = goal[2].saturating_sub(self.z[index]);
        if dx == 0 && dz == 0 {
            return;
        }
        let current = self.yaw[index] as u16 & GAME_ENTITY_YAW_MASK;
        let target = atan2_q12(dx, dz) & GAME_ENTITY_YAW_MASK;
        let clockwise = target.wrapping_sub(current) & GAME_ENTITY_YAW_MASK;
        let turn_distance = clockwise.min(4096u16.saturating_sub(clockwise));
        if turn_distance >= GAME_ENTITY_TURN_PRESENTATION_THRESHOLD {
            if self.turn_ticks[index] == 0 {
                self.turn_phase_ticks[index] = 0;
            }
            self.turn_ticks[index] = GAME_ENTITY_TURN_PRESENTATION_TICKS;
        }
        self.yaw[index] = target as i16;
    }
}

/// True when `room` is in the active window, or out of range of any
/// possible cooked room (the fail-safe keeps a mis-cooked entity
/// awake instead of frozen, hl-psx parity).
fn room_is_active(room: RoomIndex, active_rooms: &[RoomIndex]) -> bool {
    if room.raw() == u16::MAX {
        return true;
    }
    let mut i = 0usize;
    while i < active_rooms.len() {
        if active_rooms[i] == room {
            return true;
        }
        i += 1;
    }
    false
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

    const fn test_record(
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
            death_clip: 5,
            combat_capsule_first: psx_level::CombatCapsuleIndex(0),
            combat_capsule_count: 0,
            ranged_attack_action: CharacterAnimationAction::RangedAttack.to_index() as u8,
            x,
            y: 0,
            z,
            yaw: 0,
            radius: 192,
            height: 1024,
            walk_speed: 16,
            run_speed: 48,
            patrol_x: x + patrol_dx,
            patrol_y: 0,
            patrol_z: z,
            patrol_wait_ticks: 2,
            aggro_radius,
            reaction_ticks: 0,
            preferred_distance: 512,
            spacing_tolerance: 0,
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
            poise: 50,
            touch_damage: 10,
            max_health: 100,
            // Single-channel by default so every pre-existing behavior test
            // keeps its original arithmetic. `DUAL_ENEMY` below is the
            // two-channel actor the vitality tests use.
            max_health_secondary: 0,
            soul_value: 0,
            flags,
        }
    }

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
    static FAR_ROOM_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        room: RoomIndex(7),
        ..test_record(
            1000,
            1000,
            0,
            512,
            game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
        )
    }];
    static RANGED_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        aggro_radius: 4096,
        preferred_distance: 1200,
        spacing_tolerance: 100,
        attack_min_range: 500,
        attack_max_range: 1600,
        flags: game_entity_flags::ENABLED
            | game_entity_flags::CAN_RUN
            | game_entity_flags::RANGED_ATTACK,
        ..test_record(
            1000,
            1000,
            0,
            4096,
            game_entity_flags::ENABLED
                | game_entity_flags::CAN_RUN
                | game_entity_flags::RANGED_ATTACK,
        )
    }];
    static RANGED_PAIR: [LevelGameEntityRecord; 2] = [
        RANGED_ENEMY[0],
        LevelGameEntityRecord {
            z: 1200,
            patrol_z: 1200,
            ..RANGED_ENEMY[0]
        },
    ];

    const ACTIVE: [RoomIndex; 1] = [RoomIndex(0)];

    fn far_input(active_rooms: &[RoomIndex]) -> GameEntityTickInput<'_> {
        GameEntityTickInput {
            player: [100_000, 0, 100_000],
            player_room: RoomIndex(0),
            player_radius: 192,
            player_height: 1024,
            player_invulnerable: false,
            active_rooms,
        }
    }

    fn near_input(active_rooms: &[RoomIndex]) -> GameEntityTickInput<'_> {
        GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(0),
            player_radius: 192,
            player_height: 1024,
            player_invulnerable: false,
            active_rooms,
        }
    }

    /// Mover that refuses every step (a body wedged into a corner).
    struct BlockedMover;
    impl GameEntityMover for BlockedMover {
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

    impl GameEntityMover for SightMover {
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
    /// accept/hold contract the real BSP-backed mover exposes.
    #[derive(Default)]
    struct FiniteWallMover {
        calls: usize,
    }

    impl GameEntityMover for FiniteWallMover {
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

    impl GameEntityMover for CountingBlockedMover {
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
    fn empty_records_tick_is_inert() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&[]);
        let stats = entities.tick(&[], far_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.count(), 0);
        assert_eq!(stats, GameEntityTickStats::default());
    }

    #[test]
    fn spawn_copies_records_and_disabled_spawn_dead() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        assert_eq!(entities.count(), 1);
        assert_eq!(entities.state(0), GameEntityState::Idle);
        assert_eq!(entities.position(0), [1000, 0, 1000]);
        assert_eq!(entities.health(0), 100);

        entities.spawn_from_records(&DISABLED_ENEMY);
        assert_eq!(entities.state(0), GameEntityState::Dead);
    }

    #[test]
    fn hybrid_owner_chases_for_melee_then_returns_to_ranged_after_escape_margin() {
        let mut close = GameEntities::<8>::EMPTY;
        close.spawn_from_records(&RANGED_ENEMY);
        let close_input = GameEntityTickInput {
            player: [1450, 0, 1000],
            ..near_input(&ACTIVE)
        };
        close.tick(&RANGED_ENEMY, close_input, &mut NoClipMover);
        close.tick(&RANGED_ENEMY, close_input, &mut NoClipMover);
        assert_eq!(close.state(0), GameEntityState::Aggro);
        assert_eq!(close.intent(0), GameEntityIntent::Approach);
        assert!(close.position(0)[0] > 1000, "hybrid owner closes for melee");

        let still_close_input = GameEntityTickInput {
            player: [2300, 0, 1000],
            ..near_input(&ACTIVE)
        };
        let before_follow = close.position(0)[0];
        close.tick(&RANGED_ENEMY, still_close_input, &mut NoClipMover);
        assert_eq!(close.state(0), GameEntityState::Aggro);
        assert_eq!(close.intent(0), GameEntityIntent::Approach);
        assert!(
            close.position(0)[0] > before_follow,
            "the wider exit threshold keeps following instead of oscillating to ranged"
        );

        let escaped_input = GameEntityTickInput {
            player: [2500, 0, 1000],
            ..near_input(&ACTIVE)
        };
        close.tick(&RANGED_ENEMY, escaped_input, &mut NoClipMover);
        assert_eq!(close.state(0), GameEntityState::Windup);
        assert!(close.selected_attack_is_ranged(0));
        assert_eq!(
            close.selected_attack_action(&RANGED_ENEMY[0], 0),
            CharacterAnimationAction::RangedAttack
        );
    }

    #[test]
    fn close_hybrid_waiting_for_attack_slot_keeps_pursuing() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&RANGED_PAIR);
        let input = GameEntityTickInput {
            player: [1450, 0, 1000],
            ..near_input(&ACTIVE)
        };

        entities.tick(&RANGED_PAIR, input, &mut NoClipMover);
        entities.tick(&RANGED_PAIR, input, &mut NoClipMover);

        assert_eq!(entities.attack_owner(), Some(0));
        assert_eq!(entities.intent(1), GameEntityIntent::Approach);
        assert!(
            entities.position(1)[0] > 1000,
            "a close waiting enemy pressures forward instead of retreating to its firing ring"
        );
    }

    #[test]
    fn close_attacks_alternate_light_heavy_without_ranged_consuming_the_sequence() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&RANGED_ENEMY);

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
        assert_eq!(
            entities.live_position_for_model_instance(&TARGETED_ENEMY, 7),
            Some([1000, 0, 1000])
        );

        entities.x[0] = 1234;
        entities.z[0] = 876;
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
        assert_eq!(entities.count(), 2);
        assert_eq!(entities.overflow_count(), 1);
    }

    #[test]
    fn souls_attack_grammar_advances_through_windup_commit_punish() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        // Player inside aggro and attack reach (192 + 192 + 128 = 512
        // >= the 200-unit gap): Idle -> Aggro.
        let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        assert_eq!(stats.aggro_enters, 1);
        // Aggro -> Windup (in attack reach).
        let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Windup);
        assert_eq!(stats.windup_enters, 1);
        // Committing to the windup faces the player.
        assert!(entities.yaw(0) != 0, "windup facing turns toward player");
        // Windup lasts windup_ticks (3).
        let mut attack_enters = 0;
        for _ in 0..3 {
            attack_enters += entities
                .tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover)
                .attack_enters;
        }
        assert_eq!(entities.state(0), GameEntityState::Attack);
        assert_eq!(attack_enters, 1);
        // Attack window then recovery.
        let mut saw_attacking = false;
        for _ in 0..GAME_ENTITY_ATTACK_ACTIVE_TICKS {
            let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
            saw_attacking |= stats.attacking > 0;
        }
        assert!(saw_attacking);
        assert_eq!(entities.state(0), GameEntityState::Recover);
        for _ in 0..4 {
            entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        }
        assert_eq!(entities.state(0), GameEntityState::Aggro);
    }

    #[test]
    fn reaction_delay_holds_before_the_director_grants_an_attack() {
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
        entities.tick(&REACTIVE, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Aggro);

        for _ in 0..3 {
            let stats = entities.tick(&REACTIVE, near_input(&ACTIVE), &mut BlockedMover);
            assert_eq!(stats.attack_grants, 0);
            assert_eq!(entities.attack_owner(), None);
            assert_eq!(entities.state(0), GameEntityState::Aggro);
        }
        let stats = entities.tick(&REACTIVE, near_input(&ACTIVE), &mut BlockedMover);
        assert_eq!(stats.attack_grants, 1);
        assert_eq!(entities.attack_owner(), Some(0));
        assert_eq!(entities.state(0), GameEntityState::Windup);
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

        entities.tick(&REACTIVE, near_input(&ACTIVE), &mut NoClipMover);
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

        entities.tick(&REACTIVE, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.clip_for_state(&REACTIVE, 0).phase_ticks, 1);
        entities.tick(&REACTIVE, near_input(&ACTIVE), &mut NoClipMover);
        entities.tick(&REACTIVE, near_input(&ACTIVE), &mut NoClipMover);
        assert_ne!(entities.clip_for_state(&REACTIVE, 0).clip, 9);
    }

    #[test]
    fn tracking_yaw_change_plays_turn_then_settles_to_idle() {
        static TRACKING: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            aggro_radius: 1024,
            preferred_distance: 512,
            spacing_tolerance: 128,
            decision_interval_ticks: 1,
            circle_chance: 0,
            ..test_record(
                1000,
                1000,
                0,
                1024,
                game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
            )
        }];
        let input = GameEntityTickInput {
            player: [1600, 0, 1000],
            player_room: RoomIndex(0),
            player_radius: 192,
            player_height: 1024,
            player_invulnerable: false,
            active_rooms: &ACTIVE,
        };
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&TRACKING);
        entities.tick(&TRACKING, input, &mut NoClipMover);
        entities.director_delay_ticks = 100;

        entities.tick(&TRACKING, input, &mut NoClipMover);
        assert_eq!(entities.intent(0), GameEntityIntent::Hold);
        assert_eq!(
            entities.clip_for_state(&TRACKING, 0),
            GameEntityClip {
                clip: 10,
                phase_ticks: 0,
                one_shot: false,
                ..GameEntityClip::default()
            }
        );

        for _ in 0..GAME_ENTITY_TURN_PRESENTATION_TICKS {
            entities.tick(&TRACKING, input, &mut NoClipMover);
        }
        assert_eq!(entities.clip_for_state(&TRACKING, 0).clip, 0);
    }

    #[test]
    fn combat_director_grants_only_one_enemy_the_attack_slot() {
        static PAIR: [LevelGameEntityRecord; 2] = [
            test_record(
                1000,
                1000,
                0,
                512,
                game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
            ),
            test_record(
                1000,
                1000,
                0,
                512,
                game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
            ),
        ];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PAIR);
        entities.tick(&PAIR, near_input(&ACTIVE), &mut NoClipMover);
        let stats = entities.tick(&PAIR, near_input(&ACTIVE), &mut BlockedMover);

        assert_eq!(stats.attack_grants, 1);
        assert_eq!(entities.attack_owner(), Some(0));
        assert_eq!(entities.state(0), GameEntityState::Windup);
        assert_eq!(entities.state(1), GameEntityState::Aggro);
        assert_ne!(entities.intent(1), GameEntityIntent::Approach);
    }

    #[test]
    fn completed_attack_obeys_local_and_shared_cooldowns() {
        static PACED: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            attack_cooldown_ticks: 5,
            group_attack_delay_ticks: 3,
            ..test_record(
                1000,
                1000,
                0,
                512,
                game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
            )
        }];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PACED);
        entities.tick(&PACED, near_input(&ACTIVE), &mut BlockedMover);
        entities.tick(&PACED, near_input(&ACTIVE), &mut BlockedMover);
        for _ in 0..PACED[0].windup_ticks {
            entities.tick(&PACED, near_input(&ACTIVE), &mut BlockedMover);
        }
        for _ in 0..GAME_ENTITY_ATTACK_ACTIVE_TICKS {
            entities.tick(&PACED, near_input(&ACTIVE), &mut BlockedMover);
        }
        for _ in 0..PACED[0].recovery_ticks {
            entities.tick(&PACED, near_input(&ACTIVE), &mut BlockedMover);
        }
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        assert_eq!(entities.attack_owner(), None);
        assert_eq!(entities.attack_cooldown[0], 5);
        assert_eq!(entities.director_delay_ticks, 3);

        for _ in 0..4 {
            let stats = entities.tick(&PACED, near_input(&ACTIVE), &mut BlockedMover);
            assert_eq!(stats.attack_grants, 0);
            assert_eq!(entities.state(0), GameEntityState::Aggro);
        }
        let stats = entities.tick(&PACED, near_input(&ACTIVE), &mut BlockedMover);
        assert_eq!(stats.attack_grants, 1);
        assert_eq!(entities.state(0), GameEntityState::Windup);
    }

    #[test]
    fn non_attacker_retreats_when_close_and_circles_inside_its_band() {
        static SPACED: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
            aggro_radius: 2048,
            preferred_distance: 700,
            spacing_tolerance: 100,
            decision_interval_ticks: 1,
            circle_chance: 100,
            ..test_record(
                1000,
                1000,
                0,
                512,
                game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
            )
        }];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&SPACED);
        entities.state[0] = GameEntityState::Aggro as u8;
        entities.state_ticks[0] = 100;
        entities.attack_cooldown[0] = 100;
        entities.tick(&SPACED, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.intent(0), GameEntityIntent::Retreat);
        assert_eq!(
            entities.clip_for_state(&SPACED, 0).clip,
            SPACED[0].walk_backward_clip,
            "retreat uses the authored backward walk"
        );
        assert!(
            entities.position(0)[0] < 1000,
            "retreat moves away from +X player"
        );

        entities.x[0] = 1000;
        entities.z[0] = 1000;
        entities.state_ticks[0] = 100;
        entities.attack_cooldown[0] = 100;
        let in_band = GameEntityTickInput {
            player: [1700, 0, 1000],
            ..near_input(&ACTIVE)
        };
        entities.tick(&SPACED, in_band, &mut NoClipMover);
        let expected_clip = match entities.intent(0) {
            GameEntityIntent::CircleLeft => SPACED[0].strafe_left_clip,
            GameEntityIntent::CircleRight => SPACED[0].strafe_right_clip,
            other => panic!("expected circle intent, got {other:?}"),
        };
        assert_eq!(entities.clip_for_state(&SPACED, 0).clip, expected_clip);
        assert_ne!(entities.position(0)[2], 1000, "circling moves laterally");
        assert!(
            (i32::from(entities.yaw(0)) - 1024).abs() < 32,
            "circling keeps facing the player after its lateral step"
        );
    }

    #[test]
    fn clip_for_state_maps_states_and_spans_the_attack_one_shot() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        // Idle loops the idle clip from the state-entry tick.
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 0,
                phase_ticks: 0,
                one_shot: false,
                ..GameEntityClip::default()
            }
        );
        entities.tick(&IDLE_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 0,
                phase_ticks: 1,
                one_shot: false,
                ..GameEntityClip::default()
            }
        );
        // Newly acquired Aggro holds the idle clip until the director
        // grants an approach/attack intent.
        entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 0,
                phase_ticks: 0,
                one_shot: false,
                ..GameEntityClip::default()
            }
        );
        // Windup entered next tick; from there the attack clip is ONE
        // one-shot whose phase walks 1..=12 across Windup (3 ticks),
        // Attack (6 ticks), and Recover without resetting on the
        // state hops.
        entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Windup);
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 3,
                phase_ticks: 0,
                one_shot: true,
                ..GameEntityClip::default()
            }
        );
        for expected in 1..=12u16 {
            entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
            assert_eq!(
                entities.clip_for_state(&IDLE_ENEMY, 0),
                GameEntityClip {
                    clip: 3,
                    phase_ticks: expected,
                    one_shot: true,
                    ..GameEntityClip::default()
                }
            );
        }
        assert_eq!(entities.state(0), GameEntityState::Recover);
        // Stagger restarts as its own one-shot.
        entities.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 10, 60);
        assert_eq!(entities.state(0), GameEntityState::Staggered);
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 4,
                phase_ticks: 0,
                one_shot: true,
                ..GameEntityClip::default()
            }
        );
        entities.tick(&IDLE_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.clip_for_state(&IDLE_ENEMY, 0).phase_ticks, 1);
        // Death is a one-shot that keeps counting while Dead (the
        // clip finishes and holds its final frame), without waking
        // the state machine back up.
        entities.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 200, 0);
        assert_eq!(entities.state(0), GameEntityState::Dead);
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 5,
                phase_ticks: 0,
                one_shot: true,
                ..GameEntityClip::default()
            }
        );
        for _ in 0..3 {
            let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
            assert_eq!(stats.thought, 0);
        }
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 5,
                phase_ticks: 3,
                one_shot: true,
                ..GameEntityClip::default()
            }
        );
        // Out-of-range indices read inert.
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 7),
            GameEntityClip::default()
        );
    }

    #[test]
    fn aggro_deaggros_past_leash_and_patrol_walks_legs() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        // Aggro from proximity...
        entities.tick(&PATROL_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // ...then the player leaves: leash drop back to Idle.
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Idle);
        // Idle waits patrol_wait_ticks (2) then patrols to the anchor
        // 400 units away at the record's Character walk speed.
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        let stats = entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Patrol);
        assert_eq!(stats.patrol_enters, 1);
        let mut walked = 0;
        while entities.state(0) == GameEntityState::Patrol && walked < 100 {
            entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
            walked += 1;
        }
        assert_eq!(entities.state(0), GameEntityState::Idle);
        assert_eq!(entities.position(0)[0], 1400);
        // Walking the +X leg faced +X (quarter turn = 1024 PSX units).
        assert_eq!(entities.yaw(0), 1024);
    }

    #[test]
    fn chase_runs_at_run_speed_and_patrol_walks_at_walk_speed() {
        // Patrol leg: one tick moves exactly walk_speed toward the
        // anchor. Chase: one tick moves run_speed toward the player.
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Patrol);
        let before = entities.position(0)[0];
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(
            entities.position(0)[0] - before,
            PATROL_ENEMY[0].walk_speed,
            "patrol leg advances at Character walk speed"
        );

        // Fresh spawn; player inside aggro (and inside the 1024
        // leash) but outside the 512 attack reach, straight down +X:
        // the chase closes at run_speed.
        entities.spawn_from_records(&IDLE_ENEMY);
        let chase_input = GameEntityTickInput {
            player: [1800, 0, 1000],
            player_room: RoomIndex(0),
            player_radius: 192,
            player_height: 1024,
            player_invulnerable: false,
            active_rooms: &ACTIVE,
        };
        // Move the player into the 512 aggro radius first.
        let notice_input = GameEntityTickInput {
            player: [1500, 0, 1000],
            ..chase_input
        };
        entities.tick(&IDLE_ENEMY, notice_input, &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        let before = entities.position(0)[0];
        entities.tick(&IDLE_ENEMY, chase_input, &mut NoClipMover);
        assert_eq!(
            entities.position(0)[0] - before,
            IDLE_ENEMY[0].run_speed,
            "chase closes at Character run speed"
        );
    }

    #[test]
    fn chase_without_run_capability_walks_and_uses_walk_clip() {
        static WALK_ONLY_ENEMY: [LevelGameEntityRecord; 1] =
            [test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED)];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&WALK_ONLY_ENEMY);
        let chase_input = GameEntityTickInput {
            player: [1800, 0, 1000],
            player_room: RoomIndex(0),
            player_radius: 192,
            player_height: 1024,
            player_invulnerable: false,
            active_rooms: &ACTIVE,
        };
        entities.tick(
            &WALK_ONLY_ENEMY,
            GameEntityTickInput {
                player: [1500, 0, 1000],
                ..chase_input
            },
            &mut NoClipMover,
        );
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        let before = entities.position(0)[0];
        entities.tick(&WALK_ONLY_ENEMY, chase_input, &mut NoClipMover);
        assert_eq!(
            entities.position(0)[0] - before,
            WALK_ONLY_ENEMY[0].walk_speed,
            "a character without Run must approach at walk speed"
        );
        assert_eq!(
            entities.clip_for_state(&WALK_ONLY_ENEMY, 0).clip,
            WALK_ONLY_ENEMY[0].walk_clip,
            "the chase must not enter the Run clip fallback as a real action"
        );
    }

    #[test]
    fn blocked_mover_holds_position_but_state_machine_still_runs() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut BlockedMover);
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut BlockedMover);
        assert_eq!(entities.state(0), GameEntityState::Patrol);
        for _ in 0..10 {
            entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut BlockedMover);
        }
        // Fully blocked: never arrives, never leaves Patrol, position
        // pinned to spawn -- and no state corruption.
        assert_eq!(entities.state(0), GameEntityState::Patrol);
        assert_eq!(entities.position(0), [1000, 0, 1000]);
    }

    #[test]
    fn quake_chase_search_routes_patrol_around_a_finite_wall() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        entities.state[0] = GameEntityState::Patrol as u8;
        let mut mover = FiniteWallMover::default();
        let mut left_direct_line = false;

        for _ in 0..160 {
            entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut mover);
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
        assert_eq!(
            entities.position(0),
            [
                PATROL_ENEMY[0].patrol_x,
                PATROL_ENEMY[0].patrol_y,
                PATROL_ENEMY[0].patrol_z,
            ]
        );
    }

    #[test]
    fn quake_chase_search_is_deterministic_across_identical_runs() {
        let mut first = GameEntities::<8>::EMPTY;
        let mut second = GameEntities::<8>::EMPTY;
        first.spawn_from_records(&PATROL_ENEMY);
        second.spawn_from_records(&PATROL_ENEMY);
        first.state[0] = GameEntityState::Patrol as u8;
        second.state[0] = GameEntityState::Patrol as u8;
        let mut first_mover = FiniteWallMover::default();
        let mut second_mover = FiniteWallMover::default();

        for _ in 0..160 {
            first.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut first_mover);
            second.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut second_mover);
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
    fn quake_chase_search_probes_each_direction_at_most_once() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        entities.state[0] = GameEntityState::Patrol as u8;
        let mut mover = CountingBlockedMover::default();

        for _ in 0..4 {
            let before = mover.calls;
            entities.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut mover);
            assert!(
                mover.calls - before <= usize::from(GAME_ENTITY_DIRECTION_PROBES_PER_TICK),
                "the blocked search stays inside its per-tick probe budget"
            );
        }

        assert_eq!(mover.calls, 8, "every eight-way direction is probed once");
        assert_eq!(entities.position(0), [1000, 0, 1000]);
    }

    #[test]
    fn thinking_gates_on_active_rooms_with_fail_safe() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&FAR_ROOM_ENEMY);
        let near_in_room_7 = |rooms: &'static [RoomIndex]| GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(7),
            player_radius: 192,
            player_height: 1024,
            player_invulnerable: false,
            active_rooms: rooms,
        };
        // Room 7 not active: gated, no thinking.
        let stats = entities.tick(&FAR_ROOM_ENEMY, near_in_room_7(&ACTIVE), &mut NoClipMover);
        assert_eq!(stats.gated, 1);
        assert_eq!(stats.thought, 0);
        assert_eq!(entities.state(0), GameEntityState::Idle);
        // Room 7 active + player in room 7: thinks and aggros.
        static BOTH: [RoomIndex; 2] = [RoomIndex(0), RoomIndex(7)];
        let stats = entities.tick(&FAR_ROOM_ENEMY, near_in_room_7(&BOTH), &mut NoClipMover);
        assert_eq!(stats.thought, 1);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // Engaged behavior stays awake outside the active set
        // (hl-psx: combat continues outside the PVS).
        let stats = entities.tick(&FAR_ROOM_ENEMY, near_in_room_7(&ACTIVE), &mut NoClipMover);
        assert_eq!(stats.thought, 1);
    }

    #[test]
    fn owner_spatial_mask_replaces_room_gating_without_sleeping_combat() {
        static ROOM_7: [RoomIndex; 1] = [RoomIndex(7)];
        let input = GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(7),
            player_radius: 192,
            player_height: 1024,
            player_invulnerable: false,
            active_rooms: &ROOM_7,
        };
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&FAR_ROOM_ENEMY);
        entities.set_spatial_active_mask(Some(0));
        let stats = entities.tick(&FAR_ROOM_ENEMY, input, &mut NoClipMover);
        assert_eq!((stats.thought, stats.gated), (0, 1));

        entities.set_spatial_active_mask(Some(1));
        let stats = entities.tick(&FAR_ROOM_ENEMY, input, &mut NoClipMover);
        assert_eq!(stats.thought, 1);
        assert_eq!(entities.state(0), GameEntityState::Aggro);

        entities.set_spatial_active_mask(Some(0));
        let stats = entities.tick(&FAR_ROOM_ENEMY, input, &mut NoClipMover);
        assert_eq!(stats.thought, 1, "engaged behavior remains awake");
    }

    #[test]
    fn aggro_requires_matching_player_room() {
        // Same coordinates but the player is in another room: cooked
        // positions are room-local, so no notice happens.
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        let aliased = GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(3),
            player_radius: 192,
            player_height: 1024,
            player_invulnerable: false,
            active_rooms: &ACTIVE,
        };
        entities.tick(&IDLE_ENEMY, aliased, &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Idle);
    }

    #[test]
    fn line_of_sight_gates_acquisition_and_attack_commit_without_dropping_aggro() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        let mut sight = SightMover::default();

        entities.tick(&IDLE_ENEMY, far_input(&ACTIVE), &mut sight);
        assert_eq!(sight.queries, 0, "distance rejects before the BSP trace");

        entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut sight);
        assert_eq!(entities.state(0), GameEntityState::Idle);
        assert_eq!(sight.queries, 1);
        assert_eq!(sight.last_from, [1000, 512, 1000]);
        assert_eq!(sight.last_to, [1200, 512, 1000]);

        sight.clear = true;
        entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut sight);
        assert_eq!(entities.state(0), GameEntityState::Aggro);

        sight.clear = false;
        entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut sight);
        assert_eq!(
            entities.state(0),
            GameEntityState::Aggro,
            "an occluder prevents attack commitment but does not erase awareness"
        );

        sight.clear = true;
        entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut sight);
        assert_eq!(entities.state(0), GameEntityState::Windup);
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
                game_entity_flags::ENABLED | game_entity_flags::RANGED_ATTACK,
            )
        }];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PACED_ATTACK);
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
    fn hits_break_poise_then_kill() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        // Poise pool is 50: 60 poise damage staggers.
        let outcome = entities.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 10, 60);
        assert_eq!(entities.state(0), GameEntityState::Staggered);
        assert_eq!(entities.health(0), 90);
        assert!(outcome.connected && outcome.staggered && !outcome.died);
        // Stagger expires back into Aggro.
        for _ in 0..GAME_ENTITY_STAGGER_TICKS {
            entities.tick(&IDLE_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        }
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // Lethal damage kills; dead entities stop thinking.
        let outcome = entities.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 200, 0);
        assert!(outcome.connected && outcome.died);
        assert_eq!(entities.state(0), GameEntityState::Dead);
        let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(stats.thought, 0);
        // A dead entity refuses further hits.
        assert_eq!(
            entities.apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 10, 10),
            GameEntityHitOutcome::MISS
        );
    }

    /// The shipped cortex enemy shape: two equal 50-point pools.
    static EVEN_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        max_health: 50,
        max_health_secondary: 50,
        ..test_record(
            1000,
            1000,
            0,
            512,
            game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
        )
    }];

    /// Two unequal pools, so a test that reads the wrong one is obvious.
    static DUAL_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        max_health: 60,
        max_health_secondary: 40,
        ..test_record(
            1000,
            1000,
            0,
            512,
            game_entity_flags::ENABLED | game_entity_flags::CAN_RUN,
        )
    }];

    #[test]
    fn spawn_fills_both_vitality_channels() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        assert_eq!(entities.health(0), 60);
        assert_eq!(entities.health_secondary(0), 40);
        assert_eq!(entities.health_channel(0, VitalityChannelId::One), 60);
        assert_eq!(entities.health_channel(0, VitalityChannelId::Two), 40);
    }

    #[test]
    fn guarded_and_exposed_hits_use_half_and_half_again_damage() {
        let mut guarded = GameEntities::<8>::EMPTY;
        guarded.spawn_from_records(&DUAL_ENEMY);
        let hit = guarded.apply_stance_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 40, 0);
        assert!(hit.connected && !hit.died);
        assert_eq!((guarded.health(0), guarded.health_secondary(0)), (40, 40));

        let mut exposed = GameEntities::<8>::EMPTY;
        exposed.spawn_from_records(&DUAL_ENEMY);
        let hit = exposed.apply_stance_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 40, 0);
        assert!(hit.connected && !hit.died);
        // 1.5x = 60: Zenith's 40 drains first, then the remaining 20 spills.
        assert_eq!((exposed.health(0), exposed.health_secondary(0)), (40, 0));
    }

    #[test]
    fn recover_rotates_enemy_guard_and_reports_a_twelve_tick_tell() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        assert_eq!(entities.stance(0), VitalityChannelId::One);
        assert_eq!(entities.stance_swap_progress_q12(0), 4096);

        entities.enter_state(
            0,
            GameEntityState::Recover,
            &mut GameEntityTickStats::default(),
        );
        assert_eq!(entities.stance(0), VitalityChannelId::Two);
        assert_eq!(entities.stance_swap_progress_q12(0), 0);
        assert!(entities.stance_swap_in_progress(0));

        entities.advance_stance_swap(0, 6);
        assert_eq!(entities.stance_swap_progress_q12(0), 2048);
        entities.advance_stance_swap(0, 6);
        assert_eq!(entities.stance_swap_progress_q12(0), 4096);
        assert!(!entities.stance_swap_in_progress(0));
    }

    /// A hit inside the named channel's pool never touches the other one.
    /// This is the whole reason the channel is threaded down from the swing.
    #[test]
    fn a_hit_drains_only_its_own_channel() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 20, 0);
        assert_eq!((entities.health(0), entities.health_secondary(0)), (40, 40));
        entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 15, 0);
        assert_eq!((entities.health(0), entities.health_secondary(0)), (40, 25));
    }

    /// The player's own untyped path is `DualVitality::apply_spill`, so the
    /// enemy path spills the same way: only the EXCESS crosses, and it crosses
    /// in whichever direction the attack came from.
    #[test]
    fn overkill_spills_into_the_other_channel() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        // 75 against a 60 Horizon pool: 60 lands, 15 crosses into Zenith.
        let outcome = entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 75, 0);
        assert_eq!((entities.health(0), entities.health_secondary(0)), (0, 25));
        assert!(outcome.connected && !outcome.died);

        // And symmetrically, from the Zenith side on a fresh actor.
        entities.spawn_from_records(&DUAL_ENEMY);
        entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 55, 0);
        assert_eq!((entities.health(0), entities.health_secondary(0)), (45, 0));
    }

    /// Death is `DualVitality::is_defeated`: BOTH pools empty. Emptying one
    /// channel outright leaves a live actor, which is exactly why the health
    /// bar's visibility rule reads both pools too.
    #[test]
    fn death_needs_both_channels_empty() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
        let outcome = entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::Two, 40, 0);
        assert!(outcome.connected && !outcome.died);
        assert_ne!(entities.state(0), GameEntityState::Dead);
        assert_eq!((entities.health(0), entities.health_secondary(0)), (60, 0));

        let outcome = entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 59, 0);
        assert!(!outcome.died);
        assert_eq!(entities.health(0), 1);

        let outcome = entities.apply_hit(&DUAL_ENEMY, 0, VitalityChannelId::One, 1, 0);
        assert!(outcome.died);
        assert_eq!(entities.state(0), GameEntityState::Dead);
    }

    /// Total effective vitality is the sum, so a single-channel attacker kills
    /// a 60/40 actor on the same 100 damage a 100/0 actor took. That is what
    /// keeps an authored pool split from silently changing time-to-kill.
    #[test]
    fn one_channel_attacker_still_kills_at_the_summed_pool() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&DUAL_ENEMY);
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

    /// One overwhelming hit must kill outright. `u16::MAX` damage against a
    /// 50/50 actor is the exact place a spill remainder can wrap or clamp: the
    /// remainder is `damage - first_pool`, which is still far wider than the
    /// second pool, and any narrowing on the way through leaves the second
    /// channel standing and the actor alive. Reported from live combat.
    #[test]
    fn one_overwhelming_hit_kills_outright() {
        for damage in [u16::MAX, u16::MAX - 1, 60_000, 101, 100] {
            let mut entities = GameEntities::<8>::EMPTY;
            entities.spawn_from_records(&EVEN_ENEMY);
            let outcome = entities.apply_hit(&EVEN_ENEMY, 0, VitalityChannelId::One, damage, 0);
            assert!(
                outcome.connected && outcome.died,
                "{damage} damage against a 50/50 actor must kill, got {outcome:?}"
            );
            assert_eq!(entities.state(0), GameEntityState::Dead, "damage {damage}");
            assert_eq!(
                (entities.health(0), entities.health_secondary(0)),
                (0, 0),
                "damage {damage}"
            );
        }
    }

    /// One short of the combined pools must leave exactly one point standing in
    /// the SECOND channel, which pins both the spill arithmetic and the death
    /// rule against an off-by-one in either direction.
    #[test]
    fn one_short_of_the_combined_pools_does_not_kill() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&EVEN_ENEMY);
        let outcome = entities.apply_hit(&EVEN_ENEMY, 0, VitalityChannelId::One, 99, 0);
        assert!(outcome.connected && !outcome.died);
        assert_eq!((entities.health(0), entities.health_secondary(0)), (0, 1));
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
        assert!(
            entities
                .apply_hit(&EVEN_ENEMY, 0, VitalityChannelId::Two, u16::MAX, 0)
                .died
        );
        assert_eq!((entities.health(0), entities.health_secondary(0)), (0, 0));
    }

    /// The same overwhelming hit through the arc sweep, which is the other
    /// public way damage reaches a two-channel actor. A width bug in the
    /// wrapper would be invisible to the direct `apply_hit` tests above.
    #[test]
    fn an_overwhelming_arc_hit_kills_outright() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&EVEN_ENEMY);
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
        assert_eq!((entities.health(0), entities.health_secondary(0)), (0, 0));
    }

    /// A zero second pool is a legal single-channel actor: it counts as
    /// already spent, so the first pool alone decides death. Cooked projects
    /// author both channels, but the runtime contract is wider.
    #[test]
    fn a_zero_second_pool_is_a_single_channel_actor() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        assert_eq!(entities.health_secondary(0), 0);
        assert!(
            entities
                .apply_hit(&IDLE_ENEMY, 0, VitalityChannelId::One, 100, 0)
                .died
        );
        assert_eq!(entities.state(0), GameEntityState::Dead);
    }

    /// Drive IDLE_ENEMY from spawn into its Attack window against the
    /// near-input player (windup_ticks = 3): Idle -> Aggro -> Windup
    /// -> 3 windup ticks -> Attack.
    fn advance_into_attack(entities: &mut GameEntities<8>, input: GameEntityTickInput<'_>) {
        for _ in 0..5 {
            entities.tick(&IDLE_ENEMY, input, &mut NoClipMover);
        }
        assert_eq!(entities.state(0), GameEntityState::Attack);
    }

    #[test]
    fn attack_window_damages_the_player_once_per_swing() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        advance_into_attack(&mut entities, near_input(&ACTIVE));
        // First active tick connects with the record's touch damage.
        let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(stats.player_hits, 1);
        assert_eq!(stats.player_damage, IDLE_ENEMY[0].touch_damage);
        // The rest of the window and the recovery stay dry: one
        // swing, one connection. (The NEXT windup->attack loop may
        // legitimately connect again, so only the dry span is
        // walked.)
        let mut later_damage = 0u16;
        for _ in 0..8 {
            later_damage += entities
                .tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover)
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
        advance_into_attack(&mut entities, near_input(&ACTIVE));
        let rolling = GameEntityTickInput {
            player_invulnerable: true,
            ..near_input(&ACTIVE)
        };
        let mut damage = 0u16;
        for _ in 0..GAME_ENTITY_ATTACK_ACTIVE_TICKS {
            damage += entities
                .tick(&IDLE_ENEMY, rolling, &mut NoClipMover)
                .player_damage;
        }
        assert_eq!(damage, 0);
        assert_eq!(entities.state(0), GameEntityState::Recover);

        // I-framing only the first half leaves the tail live: rolling
        // too early still gets clipped (souls timing rules).
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        advance_into_attack(&mut entities, near_input(&ACTIVE));
        let early_roll = entities.tick(&IDLE_ENEMY, rolling, &mut NoClipMover);
        assert_eq!(early_roll.player_hits, 0);
        let tail = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(tail.player_hits, 1);
    }

    #[test]
    fn attacks_whiff_behind_the_committed_facing() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        // Commit the windup against a player at +X (facing locks to
        // 1024)...
        advance_into_attack(&mut entities, near_input(&ACTIVE));
        // ...then the player rolls PAST the body to -X: same reach,
        // outside the front arc, outside the point-blank ring.
        let behind = GameEntityTickInput {
            player: [800, 0, 1000],
            ..near_input(&ACTIVE)
        };
        let mut damage = 0u16;
        for _ in 0..GAME_ENTITY_ATTACK_ACTIVE_TICKS {
            damage += entities
                .tick(&IDLE_ENEMY, behind, &mut NoClipMover)
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

        stepped.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        stepped.tick(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        batched.tick_delta(&PATROL_ENEMY, far_input(&ACTIVE), &mut NoClipMover, 2);

        assert_eq!(batched.position(0), stepped.position(0));
        assert_eq!(batched.yaw(0), stepped.yaw(0));
        assert_eq!(batched.state_ticks[0], stepped.state_ticks[0]);
    }

    #[test]
    fn occluded_melee_arc_blocks_without_latching_the_swing_bit() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
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
            30,
            40,
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
            30,
            40,
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
                deaths: 0
            }
        );
        assert_eq!(entities.health(0), 85);
        assert_eq!(probed, Some((0, entities.position(0))));
        assert_ne!(swing, 0);
    }

    #[test]
    fn melee_arc_hits_once_per_swing_and_skips_dead_and_other_rooms() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
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
            60,
            40,
            &mut swing,
        );
        assert_eq!(
            stats,
            MeleeArcStats {
                hits: 1,
                staggers: 0,
                deaths: 0
            }
        );
        assert_eq!(entities.health(0), 70);
        // ...and the same swing never double-taps.
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            60,
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
            60,
            40,
            &mut swing,
        );
        assert_eq!(stats.staggers, 1);
        assert_eq!(entities.state(0), GameEntityState::Staggered);
        // Swing 3 (health 40 - 30 = 10), swing 4 kills.
        swing = 0;
        entities.apply_melee_arc(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            60,
            40,
            &mut swing,
        );
        swing = 0;
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &arc,
            VitalityChannelId::One,
            30,
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
            30,
            40,
            &mut swing,
        );
        assert_eq!(stats, MeleeArcStats::default());

        // Wrong-room arcs never connect (cooked positions are
        // room-local; a same-coordinate player in another room is an
        // alias, not a neighbor).
        entities.spawn_from_records(&IDLE_ENEMY);
        swing = 0;
        let wrong_room = MeleeArc {
            room: RoomIndex(2),
            ..arc
        };
        let stats = entities.apply_melee_arc(
            &IDLE_ENEMY,
            &wrong_room,
            VitalityChannelId::One,
            30,
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
            30,
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
            30,
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
        for _ in 0..5 {
            entities.tick_delta_deferred(
                &IDLE_ENEMY,
                near_input(&ACTIVE),
                &mut NoClipMover,
                1,
                attacks,
            );
        }
        assert_eq!(entities.state(0), GameEntityState::Attack);
        // The Windup -> Attack transition tick runs the Windup arm; the
        // first token appears on the first ACTIVE tick, not here.
        assert!(attacks.is_empty());
    }

    #[test]
    fn deferred_tokens_freeze_active_boundary_frames_and_recover_is_dry() {
        let mut entities = GameEntities::<8>::EMPTY;
        let mut attacks = DeferredGameEntityAttacks::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        advance_into_attack_deferred(&mut entities, &mut attacks);
        let windup = u16::from(IDLE_ENEMY[0].windup_ticks);

        // Every active tick freezes exactly one token whose clip/phase is the
        // attack one-shot the pose overrides play, and the deferred tick
        // never applies immediate player damage.
        for active_tick in 1..=GAME_ENTITY_ATTACK_ACTIVE_TICKS {
            let stats = entities.tick_delta_deferred(
                &IDLE_ENEMY,
                near_input(&ACTIVE),
                &mut NoClipMover,
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
            near_input(&ACTIVE),
            &mut NoClipMover,
            1,
            &mut attacks,
        );
        assert_eq!(stats.attacking, 0);
        assert!(attacks.is_empty());
    }

    #[test]
    fn deferred_tokens_reject_stale_generation_and_swing_sequence() {
        let mut entities = GameEntities::<8>::EMPTY;
        let mut attacks = DeferredGameEntityAttacks::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        advance_into_attack_deferred(&mut entities, &mut attacks);
        entities.tick_delta_deferred(
            &IDLE_ENEMY,
            near_input(&ACTIVE),
            &mut NoClipMover,
            1,
            &mut attacks,
        );
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
        assert!(!entities.deferred_attack_legacy_arc_hits(
            &IDLE_ENEMY,
            stale_swing,
            player,
            RoomIndex(0),
            192,
        ));

        // The next entity tick retires the previous tick's tokens wholesale:
        // contact may only finalize against the poses retained for the tick
        // that emitted the token.
        entities.tick_delta_deferred(
            &IDLE_ENEMY,
            near_input(&ACTIVE),
            &mut NoClipMover,
            1,
            &mut attacks,
        );
        assert!(!entities.deferred_attack_can_connect(token));
        assert!(!entities.connect_deferred_attack(token));
        assert!(!entities.deferred_attack_legacy_arc_hits(
            &IDLE_ENEMY,
            token,
            player,
            RoomIndex(0),
            192,
        ));
        let fresh = attacks.get(0).unwrap();
        assert!(entities.deferred_attack_can_connect(fresh));
    }

    #[test]
    fn deferred_connection_latches_once_and_suppresses_the_legacy_arc() {
        let mut entities = GameEntities::<8>::EMPTY;
        let mut attacks = DeferredGameEntityAttacks::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        advance_into_attack_deferred(&mut entities, &mut attacks);
        entities.tick_delta_deferred(
            &IDLE_ENEMY,
            near_input(&ACTIVE),
            &mut NoClipMover,
            1,
            &mut attacks,
        );
        let token = attacks.get(0).unwrap();
        let player = [1200, 0, 1000];

        // Legacy arc geometry (frozen origin/yaw/reach) agrees the player is
        // reachable before any connection, whiffs behind the committed
        // facing, and never crosses rooms.
        assert!(entities.deferred_attack_legacy_arc_hits(
            &IDLE_ENEMY,
            token,
            player,
            RoomIndex(0),
            192,
        ));
        assert!(!entities.deferred_attack_legacy_arc_hits(
            &IDLE_ENEMY,
            token,
            [800, 0, 1000],
            RoomIndex(0),
            192,
        ));
        assert!(!entities.deferred_attack_legacy_arc_hits(
            &IDLE_ENEMY,
            token,
            player,
            RoomIndex(2),
            192,
        ));

        // An authored-capsule connection latches the swing: the same token
        // cannot finalize twice, and the legacy arc goes dead with it, so one
        // swing can never damage through both policies.
        assert!(entities.connect_deferred_attack(token));
        assert!(!entities.connect_deferred_attack(token));
        assert!(!entities.deferred_attack_legacy_arc_hits(
            &IDLE_ENEMY,
            token,
            player,
            RoomIndex(0),
            192,
        ));

        // The latch spans the remaining active ticks of the SAME swing.
        entities.tick_delta_deferred(
            &IDLE_ENEMY,
            near_input(&ACTIVE),
            &mut NoClipMover,
            1,
            &mut attacks,
        );
        let later = attacks.get(0).unwrap();
        assert_eq!(later.swing_sequence, token.swing_sequence);
        assert!(!entities.deferred_attack_can_connect(later));
        assert!(!entities.connect_deferred_attack(later));
    }
}
