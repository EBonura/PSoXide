//! Souls-like game-entity runtime (phase 3 of
//! docs/game-runtime-plan.md): SoA state over cooked
//! [`LevelGameEntityRecord`]s with per-archetype tick dispatch across
//! the souls state grammar (Idle / Patrol / Aggro / Windup / Attack /
//! Recover / Staggered / Dead), adopted from hl-psx's prop AI shape
//! with two deliberate differences: thinking gates on the
//! portal-expanded ACTIVE ROOM set instead of BSP PVS, and melee
//! windup/commit/punish is the first-class attack grammar.
//!
//! This slice is the foundation skeleton: states advance and gate
//! correctly, movement is a straight-line placeholder step (the
//! character-motor integration and real Character-bound speeds arrive
//! with the first live archetype), and attack CONTACT resolution
//! (melee arcs vs hurtboxes) is the combat slice's job. With zero
//! cooked records every entry point returns immediately, so a
//! record-free game pays a handful of cycles per tick (measured in
//! the phase-3 budget's idle A/B).
//!
//! Crate rules hold: no statics, no unsafe, capacities are `const N`
//! parameters, cooked data arrives as `&'static` psx-level records,
//! and [`GameEntities::EMPTY`] is all-zero so the owning game can keep
//! the state in link-time-zero storage (`.bss`).

use psx_level::{game_entity_flags, LevelGameEntityRecord, RoomIndex};

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

/// Attack range placeholder for the skeleton state machine, engine
/// units in XZ. The first live archetype replaces this with
/// Character-derived reach (seam note in the phase-3 plan).
pub const GAME_ENTITY_ATTACK_RANGE: i32 = 512;

/// Straight-line placeholder step per 60 Hz tick, engine units. The
/// motor integration slice binds real Character walk speeds.
pub const GAME_ENTITY_WALK_STEP: i32 = 16;

/// Ticks the attack active window lasts in the skeleton.
pub const GAME_ENTITY_ATTACK_ACTIVE_TICKS: u16 = 6;

/// Ticks a poise break keeps the entity staggered.
pub const GAME_ENTITY_STAGGER_TICKS: u16 = 45;

/// De-aggro leash: the player escaping `aggro_radius` times this
/// factor drops the entity back to its idle/patrol loop.
pub const GAME_ENTITY_LEASH_FACTOR: i32 = 2;

/// Per-tick inputs the owning game threads in: the player pose and
/// the portal-expanded active-room set the AI gating reads.
#[derive(Clone, Copy)]
pub struct GameEntityTickInput<'a> {
    /// Player position, world/room-local engine units (the same space
    /// the cooked records use).
    pub player: [i32; 3],
    /// Room containing the player.
    pub player_room: RoomIndex,
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
            self.enter_state(index, GameEntityState::Dead);
            return;
        }
        self.poise_damage[index] = self.poise_damage[index].saturating_add(poise_damage);
        if self.poise_damage[index] > records[index].poise {
            self.poise_damage[index] = 0;
            self.enter_state(index, GameEntityState::Staggered);
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
                GameEntityState::Idle => self.tick_idle(record, index, input),
                GameEntityState::Patrol => self.tick_patrol(record, index, input),
                GameEntityState::Aggro => self.tick_aggro(record, index, input),
                GameEntityState::Windup => {
                    if self.state_ticks[index] >= u16::from(record.windup_ticks) {
                        self.enter_state(index, GameEntityState::Attack);
                    }
                }
                GameEntityState::Attack => {
                    stats.attacking += 1;
                    if self.state_ticks[index] >= GAME_ENTITY_ATTACK_ACTIVE_TICKS {
                        self.enter_state(index, GameEntityState::Recover);
                    }
                }
                GameEntityState::Recover => {
                    if self.state_ticks[index] >= u16::from(record.recovery_ticks) {
                        self.enter_state(index, GameEntityState::Aggro);
                    }
                }
                GameEntityState::Staggered => {
                    if self.state_ticks[index] >= GAME_ENTITY_STAGGER_TICKS {
                        self.enter_state(index, GameEntityState::Aggro);
                    }
                }
                GameEntityState::Dead => {}
            }
            index += 1;
        }
        stats
    }

    fn enter_state(&mut self, index: usize, state: GameEntityState) {
        self.state[index] = state as u8;
        self.state_ticks[index] = 0;
    }

    fn player_within(&self, index: usize, input: GameEntityTickInput<'_>, radius: i32) -> bool {
        within_xz(
            [self.x[index], self.z[index]],
            [input.player[0], input.player[2]],
            radius,
        )
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
    ) {
        if self.player_noticed(record, index, input) {
            self.enter_state(index, GameEntityState::Aggro);
            return;
        }
        let has_patrol = record.patrol_x != record.x
            || record.patrol_y != record.y
            || record.patrol_z != record.z;
        if has_patrol && self.state_ticks[index] >= record.patrol_wait_ticks {
            self.enter_state(index, GameEntityState::Patrol);
        }
    }

    fn tick_patrol(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
    ) {
        if self.player_noticed(record, index, input) {
            self.enter_state(index, GameEntityState::Aggro);
            return;
        }
        let goal = if self.patrol_leg[index] == 0 {
            [record.patrol_x, record.patrol_y, record.patrol_z]
        } else {
            [record.x, record.y, record.z]
        };
        if self.step_toward(index, goal, GAME_ENTITY_WALK_STEP) {
            self.patrol_leg[index] ^= 1;
            self.enter_state(index, GameEntityState::Idle);
        }
    }

    fn tick_aggro(
        &mut self,
        record: &LevelGameEntityRecord,
        index: usize,
        input: GameEntityTickInput<'_>,
    ) {
        let leash = i32::from(record.aggro_radius).saturating_mul(GAME_ENTITY_LEASH_FACTOR);
        if !self.player_within(index, input, leash) {
            // Souls de-aggro: drop the chase and return to the idle
            // loop (return-to-post pathing is the nav slice).
            self.enter_state(index, GameEntityState::Idle);
            return;
        }
        if self.player_within(index, input, GAME_ENTITY_ATTACK_RANGE) {
            self.enter_state(index, GameEntityState::Windup);
            return;
        }
        self.step_toward(index, input.player, GAME_ENTITY_WALK_STEP);
    }

    /// Straight-line XZ placeholder step (no collision -- the motor
    /// slice replaces this). Returns `true` on arrival.
    fn step_toward(&mut self, index: usize, goal: [i32; 3], step: i32) -> bool {
        let dx = goal[0].saturating_sub(self.x[index]);
        let dz = goal[2].saturating_sub(self.z[index]);
        if dx.abs() <= step && dz.abs() <= step {
            self.x[index] = goal[0];
            self.z[index] = goal[2];
            return true;
        }
        self.x[index] = self.x[index].saturating_add(dx.clamp(-step, step));
        self.z[index] = self.z[index].saturating_add(dz.clamp(-step, step));
        false
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
            active_rooms,
        }
    }

    fn near_input(active_rooms: &[RoomIndex]) -> GameEntityTickInput<'_> {
        GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(0),
            active_rooms,
        }
    }

    #[test]
    fn empty_records_tick_is_inert() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&[]);
        let stats = entities.tick(&[], far_input(&ACTIVE));
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
        // Player inside aggro and attack range: Idle -> Aggro.
        entities.tick(&IDLE_ENEMY, near_input(&ACTIVE));
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // Aggro -> Windup (in attack range).
        entities.tick(&IDLE_ENEMY, near_input(&ACTIVE));
        assert_eq!(entities.state(0), GameEntityState::Windup);
        // Windup lasts windup_ticks (3).
        for _ in 0..3 {
            entities.tick(&IDLE_ENEMY, near_input(&ACTIVE));
        }
        assert_eq!(entities.state(0), GameEntityState::Attack);
        // Attack window then recovery.
        let mut saw_attacking = false;
        for _ in 0..GAME_ENTITY_ATTACK_ACTIVE_TICKS {
            let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE));
            saw_attacking |= stats.attacking > 0;
        }
        assert!(saw_attacking);
        assert_eq!(entities.state(0), GameEntityState::Recover);
        for _ in 0..4 {
            entities.tick(&IDLE_ENEMY, near_input(&ACTIVE));
        }
        assert_eq!(entities.state(0), GameEntityState::Aggro);
    }

    #[test]
    fn aggro_deaggros_past_leash_and_patrol_walks_legs() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&PATROL_ENEMY);
        // Aggro from proximity...
        entities.tick(&PATROL_ENEMY, near_input(&ACTIVE));
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // ...then the player leaves: leash drop back to Idle.
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE));
        assert_eq!(entities.state(0), GameEntityState::Idle);
        // Idle waits patrol_wait_ticks (2) then patrols to the anchor
        // 400 units away at GAME_ENTITY_WALK_STEP per tick.
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE));
        entities.tick(&PATROL_ENEMY, far_input(&ACTIVE));
        assert_eq!(entities.state(0), GameEntityState::Patrol);
        let mut walked = 0;
        while entities.state(0) == GameEntityState::Patrol && walked < 100 {
            entities.tick(&PATROL_ENEMY, far_input(&ACTIVE));
            walked += 1;
        }
        assert_eq!(entities.state(0), GameEntityState::Idle);
        assert_eq!(entities.position(0)[0], 1400);
    }

    #[test]
    fn thinking_gates_on_active_rooms_with_fail_safe() {
        let mut entities = GameEntities::<8>::EMPTY;
        entities.spawn_from_records(&FAR_ROOM_ENEMY);
        let near_in_room_7 = |rooms: &'static [RoomIndex]| GameEntityTickInput {
            player: [1200, 0, 1000],
            player_room: RoomIndex(7),
            active_rooms: rooms,
        };
        // Room 7 not active: gated, no thinking.
        let stats = entities.tick(&FAR_ROOM_ENEMY, near_in_room_7(&ACTIVE));
        assert_eq!(stats.gated, 1);
        assert_eq!(stats.thought, 0);
        assert_eq!(entities.state(0), GameEntityState::Idle);
        // Room 7 active + player in room 7: thinks and aggros.
        static BOTH: [RoomIndex; 2] = [RoomIndex(0), RoomIndex(7)];
        let stats = entities.tick(&FAR_ROOM_ENEMY, near_in_room_7(&BOTH));
        assert_eq!(stats.thought, 1);
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // Engaged behavior stays awake outside the active set
        // (hl-psx: combat continues outside the PVS).
        let stats = entities.tick(&FAR_ROOM_ENEMY, near_in_room_7(&ACTIVE));
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
            active_rooms: &ACTIVE,
        };
        entities.tick(&IDLE_ENEMY, aliased);
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
            entities.tick(&IDLE_ENEMY, far_input(&ACTIVE));
        }
        assert_eq!(entities.state(0), GameEntityState::Aggro);
        // Lethal damage kills; dead entities stop thinking.
        entities.apply_hit(&IDLE_ENEMY, 0, 200, 0);
        assert_eq!(entities.state(0), GameEntityState::Dead);
        let stats = entities.tick(&IDLE_ENEMY, near_input(&ACTIVE));
        assert_eq!(stats.thought, 0);
    }
}
