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
//! stand/slide/step rules the player uses. Attack CONTACT resolution
//! is the combat slice (see [`crate::combat`]): an entity's Attack
//! active window tests the player against its Character-derived reach
//! inside a front arc (once per swing, whiffing on roll i-frames),
//! and the player's swings come back through [`Self::apply_melee_arc`].
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
use psx_level::{game_entity_flags, LevelGameEntityRecord, RoomIndex};
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
pub const GAME_ENTITY_ATTACK_REACH_MARGIN: i32 = 128;

/// Ticks the attack active window lasts.
pub const GAME_ENTITY_ATTACK_ACTIVE_TICKS: u16 = 6;

/// Front-arc half-width for entity attacks, PSX angle units
/// (60 degrees). The entity faced the player when it committed to
/// its windup; a player who rolls past the body during the swing
/// leaves this arc and the attack whiffs -- the souls punish loop.
pub const GAME_ENTITY_ATTACK_HALF_ANGLE: u16 = 683;

/// Ticks a poise break keeps the entity staggered.
pub const GAME_ENTITY_STAGGER_TICKS: u16 = 45;

/// De-aggro leash: the player escaping `aggro_radius` times this
/// factor drops the entity back to its idle/patrol loop.
pub const GAME_ENTITY_LEASH_FACTOR: i32 = 2;

/// Quarter turn in the engine's 4096-unit yaw representation.
const GAME_ENTITY_QUARTER_TURN: u16 = 1024;

/// Half turn in the engine's 4096-unit yaw representation.
const GAME_ENTITY_HALF_TURN: u16 = 2048;

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameEntityClip {
    /// Model-local clip index from the cooked record.
    pub clip: u16,
    /// 60 Hz ticks into the clip's playback.
    pub phase_ticks: u16,
    /// One-shot playback: clamp at the clip's final frame instead of
    /// looping.
    pub one_shot: bool,
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
    /// Remaining health.
    health: [u16; MAX_ENTITIES],
    /// Accumulated poise damage (staggers past the record's pool).
    poise_damage: [u16; MAX_ENTITIES],
    /// Patrol leg: 0 = toward the patrol anchor, 1 = toward spawn.
    patrol_leg: [u8; MAX_ENTITIES],
    /// 1 while the current Attack window already connected (one hit
    /// per swing); cleared on entering Attack.
    attack_hit: [u8; MAX_ENTITIES],
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
}

impl<const MAX_ENTITIES: usize> GameEntities<MAX_ENTITIES> {
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
        health: [0; MAX_ENTITIES],
        poise_damage: [0; MAX_ENTITIES],
        patrol_leg: [0; MAX_ENTITIES],
        attack_hit: [0; MAX_ENTITIES],
        intent: [0; MAX_ENTITIES],
        attack_cooldown: [0; MAX_ENTITIES],
        attack_wait_ticks: [0; MAX_ENTITIES],
        attack_owner_plus_one: 0,
        director_delay_ticks: 0,
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
            self.poise_damage[index] = 0;
            self.patrol_leg[index] = 0;
            self.state_ticks[index] = 0;
            self.intent[index] = GameEntityIntent::Hold as u8;
            self.attack_cooldown[index] = 0;
            self.attack_wait_ticks[index] = 0;
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

    /// Remaining health of entity `index`.
    pub fn health(&self, index: usize) -> u16 {
        if index >= self.count() {
            0
        } else {
            self.health[index]
        }
    }

    /// Clip selection for entity `index`'s current state. Locomotion
    /// states loop from their state-entry tick; the attack grammar
    /// plays the attack clip as ONE one-shot whose phase spans Windup
    /// + Attack + Recover (the telegraph/commit/punish the player
    /// reads is the same clip the AI windows run on). Stagger and
    /// Death are one-shots from state entry; Dead keeps counting
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
        let looping = |clip: u16| GameEntityClip {
            clip,
            phase_ticks: ticks,
            one_shot: false,
        };
        let one_shot = |clip: u16, phase_ticks: u16| GameEntityClip {
            clip,
            phase_ticks,
            one_shot: true,
        };
        match self.state(index) {
            GameEntityState::Idle => looping(record.idle_clip),
            GameEntityState::Patrol => looping(record.walk_clip),
            GameEntityState::Aggro => match self.intent(index) {
                GameEntityIntent::Approach => looping(record.run_clip),
                GameEntityIntent::CircleLeft
                | GameEntityIntent::CircleRight
                | GameEntityIntent::Retreat => looping(record.walk_clip),
                GameEntityIntent::Hold => looping(record.idle_clip),
            },
            GameEntityState::Windup => one_shot(record.attack_clip, ticks),
            GameEntityState::Attack => one_shot(
                record.attack_clip,
                u16::from(record.windup_ticks).saturating_add(ticks),
            ),
            GameEntityState::Recover => one_shot(
                record.attack_clip,
                u16::from(record.windup_ticks)
                    .saturating_add(GAME_ENTITY_ATTACK_ACTIVE_TICKS)
                    .saturating_add(ticks),
            ),
            GameEntityState::Staggered => one_shot(record.stagger_clip, ticks),
            GameEntityState::Dead => one_shot(record.death_clip, ticks),
        }
    }

    /// Apply a hit to entity `index`: health damage plus poise
    /// damage. Health reaching zero kills; accumulated poise damage
    /// past the record's pool staggers (and the accumulator resets).
    /// Combat resolution routes every player weapon hit through here
    /// (usually via [`Self::apply_melee_arc`]).
    pub fn apply_hit(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        damage: u16,
        poise_damage: u16,
    ) -> GameEntityHitOutcome {
        if index >= self.count() || index >= records.len() {
            return GameEntityHitOutcome::MISS;
        }
        if self.state(index) == GameEntityState::Dead {
            return GameEntityHitOutcome::MISS;
        }
        self.health[index] = self.health[index].saturating_sub(damage);
        if self.health[index] == 0 {
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
    /// [`Self::apply_hit`], and its bit latches so one swing connects
    /// at most once per enemy. `O(live entities)` with the
    /// per-axis/squared early-outs of [`arc_hits_circle`]; the owning
    /// game clears `already_hit` when a new swing starts. Bit `i`
    /// tracks entity `i` (the [`psx_level::MAX_GAME_ENTITY_RECORDS`]
    /// = 64 contract cap is exactly the u64 width).
    pub fn apply_melee_arc(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        arc: &MeleeArc,
        damage: u16,
        poise_damage: u16,
        // psx-numeric-allow-next-line: one-hit-per-swing bitmask; bit ops only, two-word on R3000
        already_hit: &mut u64,
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
                );
            if !skip {
                *already_hit |= mask;
                let outcome = self.apply_hit(records, entity, damage, poise_damage);
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
        let delta_ticks = delta_ticks.max(1);
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
            let behavior_awake = !matches!(state, GameEntityState::Idle | GameEntityState::Patrol);
            if !behavior_awake && !room_is_active(record.room, input.active_rooms) {
                stats.gated += 1;
                index += 1;
                continue;
            }
            stats.thought += 1;
            self.state_ticks[index] = self.state_ticks[index].saturating_add(delta_ticks);
            match state {
                GameEntityState::Idle => self.tick_idle(record, index, input, &mut stats),
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
                    self.resolve_attack_contact(record, index, input, &mut stats);
                    if self.state_ticks[index] >= GAME_ENTITY_ATTACK_ACTIVE_TICKS {
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
        if state != GameEntityState::Aggro {
            self.set_intent(index, GameEntityIntent::Hold);
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
                self.attack_hit[index] = 0;
            }
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
        if self.attack_hit[index] != 0
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
            reach: Self::attack_reach(record, input),
            half_angle: GAME_ENTITY_ATTACK_HALF_ANGLE,
        };
        // The player hurtbox center is the motor position; its radius
        // is already inside `attack_reach` (radius + radius + margin),
        // so the arc tests the CENTER (radius 0) to avoid counting the
        // player capsule twice.
        if !arc_hits_circle(&arc, input.player[0], input.player[2], 0) {
            return;
        }
        self.attack_hit[index] = 1;
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

    /// Character-derived melee reach: both body radii plus the
    /// close-in margin (see [`GAME_ENTITY_ATTACK_REACH_MARGIN`]).
    fn attack_reach(record: &LevelGameEntityRecord, input: GameEntityTickInput<'_>) -> i32 {
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
    ) -> bool {
        input.player_room == record.room
            && self.player_within(index, input, i32::from(record.aggro_radius))
    }

    fn tick_idle(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
        stats: &mut GameEntityTickStats,
    ) {
        if self.player_noticed(record, index, input) {
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
        if self.player_noticed(record, index, input) {
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
            stats.holding = stats.holding.saturating_add(1);
            return;
        }

        if self.attack_owner() == Some(index) {
            self.set_intent(index, GameEntityIntent::Approach);
            if self.player_within(index, input, Self::attack_reach(record, input)) {
                self.face_toward(index, input.player);
                self.enter_state(index, GameEntityState::Windup, stats);
                return;
            }
            self.step_toward(
                record,
                index,
                input.player,
                record.run_speed.saturating_mul(i32::from(delta_ticks)),
                mover,
            );
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
                record.run_speed.saturating_mul(i32::from(delta_ticks)),
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
        let sin = i32::from(psx_math::sin_q12(move_yaw));
        let cos = i32::from(psx_math::cos_q12(move_yaw));
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
    /// units per tick. The step direction quantizes through the same
    /// Q12 yaw sin/cos the player motor uses, the entity faces its
    /// movement, and the committed position (full, slid, or held) is
    /// whatever the mover's collision allows. Returns `true` on
    /// arrival (within one step of the goal and the final hop
    /// committed).
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
        self.face_toward(index, goal);
        let arriving = dx.abs() <= speed && dz.abs() <= speed;
        let (step_x, step_z) = if arriving {
            (dx, dz)
        } else {
            // The player motor's move shape: yaw -> Q12 sin/cos * speed.
            let yaw = atan2_q12(dx, dz);
            let sin = i32::from(psx_math::sin_q12(yaw));
            let cos = i32::from(psx_math::cos_q12(yaw));
            ((sin * speed) >> 12, (cos * speed) >> 12)
        };
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
        arriving && committed[0] == goal[0] && committed[2] == goal[2]
    }

    /// Face the XZ direction toward `goal` (PSX angle units, the
    /// motor's yaw convention: x = sin, z = cos).
    fn face_toward(&mut self, index: usize, goal: [i32; 3]) {
        let dx = goal[0].saturating_sub(self.x[index]);
        let dz = goal[2].saturating_sub(self.z[index]);
        if dx == 0 && dz == 0 {
            return;
        }
        self.yaw[index] = atan2_q12(dx, dz) as i16;
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
            walk_clip: 1,
            run_clip: 2,
            attack_clip: 3,
            stagger_clip: 4,
            death_clip: 5,
            combat_capsule_first: psx_level::CombatCapsuleIndex(0),
            combat_capsule_count: 0,
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
            recovery_ticks: 4,
            poise: 50,
            touch_damage: 10,
            max_health: 100,
            flags,
        }
    }

    static IDLE_ENEMY: [LevelGameEntityRecord; 1] =
        [test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED)];
    static PATROL_ENEMY: [LevelGameEntityRecord; 1] = [test_record(
        1000,
        1000,
        400,
        512,
        game_entity_flags::ENABLED,
    )];
    static TARGETED_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        model_instance: 7,
        ..test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED)
    }];
    static DISABLED_ENEMY: [LevelGameEntityRecord; 1] = [test_record(0, 0, 0, 512, 0)];
    static FAR_ROOM_ENEMY: [LevelGameEntityRecord; 1] = [LevelGameEntityRecord {
        room: RoomIndex(7),
        ..test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED)
    }];

    const ACTIVE: [RoomIndex; 1] = [RoomIndex(0)];

    fn far_input(active_rooms: &[RoomIndex]) -> GameEntityTickInput<'_> {
        GameEntityTickInput {
            player: [100_000, 0, 100_000],
            player_room: RoomIndex(0),
            player_radius: 192,
            player_invulnerable: false,
            active_rooms,
        }
    }

    fn near_input(active_rooms: &[RoomIndex]) -> GameEntityTickInput<'_> {
        GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(0),
            player_radius: 192,
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
            test_record(0, 0, 0, 512, game_entity_flags::ENABLED),
            test_record(100, 0, 0, 512, game_entity_flags::ENABLED),
            test_record(200, 0, 0, 512, game_entity_flags::ENABLED),
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
            ..test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED)
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
    fn combat_director_grants_only_one_enemy_the_attack_slot() {
        static PAIR: [LevelGameEntityRecord; 2] = [
            test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED),
            test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED),
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
            ..test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED)
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
            ..test_record(1000, 1000, 0, 512, game_entity_flags::ENABLED)
        }];
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&SPACED);
        entities.state[0] = GameEntityState::Aggro as u8;
        entities.state_ticks[0] = 100;
        entities.attack_cooldown[0] = 100;
        entities.tick(&SPACED, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.intent(0), GameEntityIntent::Retreat);
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
        assert!(matches!(
            entities.intent(0),
            GameEntityIntent::CircleLeft | GameEntityIntent::CircleRight
        ));
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
                one_shot: false
            }
        );
        entities.tick(&IDLE_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 0,
                phase_ticks: 1,
                one_shot: false
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
                one_shot: false
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
                one_shot: true
            }
        );
        for expected in 1..=12u16 {
            entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
            assert_eq!(
                entities.clip_for_state(&IDLE_ENEMY, 0),
                GameEntityClip {
                    clip: 3,
                    phase_ticks: expected,
                    one_shot: true
                }
            );
        }
        assert_eq!(entities.state(0), GameEntityState::Recover);
        // Stagger restarts as its own one-shot.
        entities.apply_hit(&IDLE_ENEMY, 0, 10, 60);
        assert_eq!(entities.state(0), GameEntityState::Staggered);
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 4,
                phase_ticks: 0,
                one_shot: true
            }
        );
        entities.tick(&IDLE_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(entities.clip_for_state(&IDLE_ENEMY, 0).phase_ticks, 1);
        // Death is a one-shot that keeps counting while Dead (the
        // clip finishes and holds its final frame), without waking
        // the state machine back up.
        entities.apply_hit(&IDLE_ENEMY, 0, 200, 0);
        assert_eq!(entities.state(0), GameEntityState::Dead);
        assert_eq!(
            entities.clip_for_state(&IDLE_ENEMY, 0),
            GameEntityClip {
                clip: 5,
                phase_ticks: 0,
                one_shot: true
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
                one_shot: true
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
    fn thinking_gates_on_active_rooms_with_fail_safe() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&FAR_ROOM_ENEMY);
        let near_in_room_7 = |rooms: &'static [RoomIndex]| GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(7),
            player_radius: 192,
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
    fn aggro_requires_matching_player_room() {
        // Same coordinates but the player is in another room: cooked
        // positions are room-local, so no notice happens.
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        let aliased = GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(3),
            player_radius: 192,
            player_invulnerable: false,
            active_rooms: &ACTIVE,
        };
        entities.tick(&IDLE_ENEMY, aliased, &mut NoClipMover);
        assert_eq!(entities.state(0), GameEntityState::Idle);
    }

    #[test]
    fn hits_break_poise_then_kill() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&IDLE_ENEMY);
        // Poise pool is 50: 60 poise damage staggers.
        let outcome = entities.apply_hit(&IDLE_ENEMY, 0, 10, 60);
        assert_eq!(entities.state(0), GameEntityState::Staggered);
        assert_eq!(entities.health(0), 90);
        assert!(outcome.connected && outcome.staggered && !outcome.died);
        // Stagger expires back into Aggro.
        for _ in 0..GAME_ENTITY_STAGGER_TICKS {
            entities.tick(&IDLE_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        }
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // Lethal damage kills; dead entities stop thinking.
        let outcome = entities.apply_hit(&IDLE_ENEMY, 0, 200, 0);
        assert!(outcome.connected && outcome.died);
        assert_eq!(entities.state(0), GameEntityState::Dead);
        let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(stats.thought, 0);
        // A dead entity refuses further hits.
        assert_eq!(
            entities.apply_hit(&IDLE_ENEMY, 0, 10, 10),
            GameEntityHitOutcome::MISS
        );
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
        for _ in 0..u16::from(GAME_ENTITY_ATTACK_ACTIVE_TICKS) {
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
        for _ in 0..u16::from(GAME_ENTITY_ATTACK_ACTIVE_TICKS) {
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
        let stats = entities.apply_melee_arc(&IDLE_ENEMY, &arc, 30, 40, &mut swing);
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
        let stats = entities.apply_melee_arc(&IDLE_ENEMY, &arc, 30, 40, &mut swing);
        assert_eq!(stats, MeleeArcStats::default());
        // Swing 2: accumulated poise (40 + 40) breaks the 50 pool.
        swing = 0;
        let stats = entities.apply_melee_arc(&IDLE_ENEMY, &arc, 30, 40, &mut swing);
        assert_eq!(stats.staggers, 1);
        assert_eq!(entities.state(0), GameEntityState::Staggered);
        // Swing 3 (health 40 - 30 = 10), swing 4 kills.
        swing = 0;
        entities.apply_melee_arc(&IDLE_ENEMY, &arc, 30, 40, &mut swing);
        swing = 0;
        let stats = entities.apply_melee_arc(&IDLE_ENEMY, &arc, 30, 40, &mut swing);
        assert_eq!(stats.deaths, 1);
        assert_eq!(entities.state(0), GameEntityState::Dead);
        // Dead entities are no longer targets.
        swing = 0;
        let stats = entities.apply_melee_arc(&IDLE_ENEMY, &arc, 30, 40, &mut swing);
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
        let stats = entities.apply_melee_arc(&IDLE_ENEMY, &wrong_room, 30, 40, &mut swing);
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
        let stats = entities.apply_melee_arc(&IDLE_ENEMY, &away, 30, 40, &mut swing);
        assert_eq!(stats, MeleeArcStats::default());
        swing = 0;
        let toward = MeleeArc { yaw: 3072, ..away };
        let stats = entities.apply_melee_arc(&IDLE_ENEMY, &toward, 30, 40, &mut swing);
        assert_eq!(stats.hits, 1);
    }
}
