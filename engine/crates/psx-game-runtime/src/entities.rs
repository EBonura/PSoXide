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
//! (melee arcs vs hurtboxes) is the combat slice's job. With zero
//! cooked records every entry point returns immediately, so a
//! record-free game pays a handful of cycles per tick (measured in
//! the phase-3 budget's idle A/B).
//!
//! Crate rules hold: no statics, no unsafe, capacities are `const N`
//! parameters, cooked data arrives as `&'static` psx-level records,
//! and [`GameEntities::EMPTY`] is all-zero so the owning game can keep
//! the state in link-time-zero storage (`.bss`).
//!
//! [`commit_body_step`]: psx_engine::character_motor::commit_body_step

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

/// Melee close-in margin added on top of the two body radii when
/// deriving attack reach: an entity commits to its windup when the
/// player is within `record.radius + player_radius + MARGIN` in XZ.
/// The radii are Character-bound (cooked from the same
/// `CharacterControllerSettings` the motors use); this constant is the
/// one runtime tuning knob (roughly half a demo-scale step) standing
/// in for per-weapon reach until the combat slice cooks real melee
/// arcs.
pub const GAME_ENTITY_ATTACK_REACH_MARGIN: i32 = 128;

/// Ticks the attack active window lasts in the skeleton.
pub const GAME_ENTITY_ATTACK_ACTIVE_TICKS: u16 = 6;

/// Ticks a poise break keeps the entity staggered.
pub const GAME_ENTITY_STAGGER_TICKS: u16 = 45;

/// De-aggro leash: the player escaping `aggro_radius` times this
/// factor drops the entity back to its idle/patrol loop.
pub const GAME_ENTITY_LEASH_FACTOR: i32 = 2;

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

    /// Position of entity `index`, room-local engine units.
    pub fn position(&self, index: usize) -> [i32; 3] {
        if index >= self.count() {
            return [0; 3];
        }
        [self.x[index], self.y[index], self.z[index]]
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

    /// Apply a hit to entity `index`: health damage plus poise
    /// damage. Health reaching zero kills; accumulated poise damage
    /// past the record's pool staggers (and the accumulator resets).
    /// The combat-resolution slice routes weapon hits through here.
    pub fn apply_hit(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        damage: u16,
        poise_damage: u16,
    ) {
        if index >= self.count() || index >= records.len() {
            return;
        }
        if self.state(index) == GameEntityState::Dead {
            return;
        }
        self.health[index] = self.health[index].saturating_sub(damage);
        if self.health[index] == 0 {
            self.enter_state(
                index,
                GameEntityState::Dead,
                &mut GameEntityTickStats::default(),
            );
            return;
        }
        self.poise_damage[index] = self.poise_damage[index].saturating_add(poise_damage);
        if self.poise_damage[index] > records[index].poise {
            self.poise_damage[index] = 0;
            self.enter_state(
                index,
                GameEntityState::Staggered,
                &mut GameEntityTickStats::default(),
            );
        }
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
        let mut stats = GameEntityTickStats::default();
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            let record = &records[index];
            let state = GameEntityState::from_raw(self.state[index]);
            if state == GameEntityState::Dead {
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
            self.state_ticks[index] = self.state_ticks[index].saturating_add(1);
            match state {
                GameEntityState::Idle => self.tick_idle(record, index, input, &mut stats),
                GameEntityState::Patrol => {
                    self.tick_patrol(record, index, input, mover, &mut stats)
                }
                GameEntityState::Aggro => self.tick_aggro(record, index, input, mover, &mut stats),
                GameEntityState::Windup => {
                    if self.state_ticks[index] >= u16::from(record.windup_ticks) {
                        self.enter_state(index, GameEntityState::Attack, &mut stats);
                    }
                }
                GameEntityState::Attack => {
                    stats.attacking += 1;
                    if self.state_ticks[index] >= GAME_ENTITY_ATTACK_ACTIVE_TICKS {
                        self.enter_state(index, GameEntityState::Recover, &mut stats);
                    }
                }
                GameEntityState::Recover => {
                    if self.state_ticks[index] >= u16::from(record.recovery_ticks) {
                        self.enter_state(index, GameEntityState::Aggro, &mut stats);
                    }
                }
                GameEntityState::Staggered => {
                    if self.state_ticks[index] >= GAME_ENTITY_STAGGER_TICKS {
                        self.enter_state(index, GameEntityState::Aggro, &mut stats);
                    }
                }
                GameEntityState::Dead => {}
            }
            index += 1;
        }
        stats
    }

    fn enter_state(
        &mut self,
        index: usize,
        state: GameEntityState,
        stats: &mut GameEntityTickStats,
    ) {
        self.state[index] = state as u8;
        self.state_ticks[index] = 0;
        match state {
            GameEntityState::Patrol => stats.patrol_enters += 1,
            GameEntityState::Aggro => stats.aggro_enters += 1,
            GameEntityState::Windup => stats.windup_enters += 1,
            GameEntityState::Attack => stats.attack_enters += 1,
            _ => {}
        }
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
        if self.step_toward(record, index, goal, record.walk_speed, mover) {
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
        stats: &mut GameEntityTickStats,
    ) {
        let leash = i32::from(record.aggro_radius).saturating_mul(GAME_ENTITY_LEASH_FACTOR);
        if !self.player_within(index, input, leash) {
            // Souls de-aggro: drop the chase and return to the idle
            // loop (return-to-post pathing is the nav slice).
            self.enter_state(index, GameEntityState::Idle, stats);
            return;
        }
        if self.player_within(index, input, Self::attack_reach(record, input)) {
            self.face_toward(index, input.player);
            self.enter_state(index, GameEntityState::Windup, stats);
            return;
        }
        self.step_toward(record, index, input.player, record.run_speed, mover);
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
            active_rooms,
        }
    }

    fn near_input(active_rooms: &[RoomIndex]) -> GameEntityTickInput<'_> {
        GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(0),
            player_radius: 192,
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
        entities.apply_hit(&IDLE_ENEMY, 0, 10, 60);
        assert_eq!(entities.state(0), GameEntityState::Staggered);
        assert_eq!(entities.health(0), 90);
        // Stagger expires back into Aggro.
        for _ in 0..GAME_ENTITY_STAGGER_TICKS {
            entities.tick(&IDLE_ENEMY, far_input(&ACTIVE), &mut NoClipMover);
        }
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // Lethal damage kills; dead entities stop thinking.
        entities.apply_hit(&IDLE_ENEMY, 0, 200, 0);
        assert_eq!(entities.state(0), GameEntityState::Dead);
        let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE), &mut NoClipMover);
        assert_eq!(stats.thought, 0);
    }
}
