//! The arena: two fighters, each an entity of its own one-slot table, each
//! driven by a controller (the enemy tactical AI, or a scripted pad through
//! the possession seam), fighting on the shared rules, the shared motor and
//! the shared projectile pool.
//!
//! What it runs from the game, unchanged: `GameEntities` (state machine,
//! tactical controller, `apply_intent`, `Combatant` rules: parry, crit,
//! knockdown, poise, energy), the character motor, `CombatProjectiles`, and
//! the one landing (`GameEntities::receive_hit` on the victim, then
//! `GameEntities::answer_strike` on the attacker). Each fighter is posed
//! every tick by the game's own model loader and pose resolver
//! ([`crate::pose`]), and contact is the disc's: its authored hitboxes
//! swept over the tick against the other's authored hurtboxes
//! (`combat::ActiveStrike`), a bolt against the first authored hurtbox.
//! What it adds: the owner's side of a duel (perception of the other
//! fighter, contact in both directions, bolt release), which on the disc is
//! `Playtest`.
//!
//! Both fighters are treated the same in every step: each thinks from a
//! snapshot of the other taken before either moved, contacts in both
//! directions are latched before either lands, so swapping the two sides
//! (with their seeds) gives the same fight mirrored.

use std::fmt::Write as _;

use psx_engine::{
    Angle, CharacterBlockerTraceProvider, CharacterCollisionAabb, CharacterCollisionCylinder,
    CharacterMotorConfig, CharacterMotorFrame, CharacterMotorInput, CharacterMotorState, RoomPoint,
};
use psx_game_runtime::combat::{
    authored_projectile_release_pending_at_frame, body_hurt_capsule, projectile_hurtboxes,
    ticks_to_contact, ActiveStrike, WorldCombatCapsule,
};
use psx_game_runtime::combat_intent::{CombatAction, CombatActions, CombatIntent, CombatMove};
use psx_game_runtime::combatant::{Combatant, CombatantId, Defender, HitKind, Reaction, Strike};
use psx_game_runtime::entities::{
    melee_swing_reach, AttackPhase, CombatTarget, DeferredGameEntityAttacks, FighterWorld,
    GameEntities, GameEntityState, GameEntityTickInput, OpenSpace,
};
use psx_game_runtime::projectiles::{
    CombatProjectiles, CombatTeam, ProjectileDamageChannel, ProjectileImpactKind,
    ProjectileImpacts, ProjectileSpawn, ProjectileTarget, ProjectileVisualStyle,
    ProjectileWorldTrace, ProjectileWorldTracer,
};
use psx_game_runtime::vitality::VitalityChannelId;
use psx_level::{CharacterAnimationAction, LevelCharacterRecord, LevelGameEntityRecord, RoomIndex};

use crate::fighter::FighterSpec;

/// One fighter's entity table: one slot, stance-bound attacks, tactical.
pub type Entities = GameEntities<1>;

/// The disc duel's time limit (`duel_observe`: three minutes).
pub const TIME_LIMIT_TICKS: u32 = 10_800;
/// The disc duel's stall rule: this long without damage ends the fight.
pub const STALL_TICKS: u32 = 1_800;
/// Entities think at 30 Hz with a two-tick delta, as on the disc.
pub const THINK_DELTA: u16 = 2;
/// The game's speed knob every fighter runs (`PLAYER_SPEED_SCALE_*` in the
/// editor playtest: 3/4).
pub const SPEED_SCALE: (i32, i32) = (3, 4);

/// Set the code-constant `DRY_CYCLES` placeholder for every following run
/// (host harness setting; process global, so sweeps that change it run one
/// configuration at a time).
pub fn set_dry_cycles(cycles: u8) {
    psx_game_runtime::ranged_tactics::DRY_CYCLES_SETTING
        .store(cycles, std::sync::atomic::Ordering::Relaxed);
}

/// Set the downed rule the fighters run (`combatant::DOWNED_PROTECTED`, a
/// code constant the `harness` feature makes a setting).
pub fn set_downed_protected(protected: bool) {
    psx_game_runtime::combatant::DOWNED_PROTECTED_SETTING
        .store(protected, std::sync::atomic::Ordering::Relaxed);
}

/// Who drives a fighter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Controller {
    /// The enemy tactical AI (`decide_tactical`): the one AI controller.
    /// On the disc the player's side is the duel bot, which shares every
    /// decision function with it; here Aletha is driven by this too.
    Ai,
    /// A scripted pad: a fixed button pattern, fed through the possession
    /// seam (`FighterWorld::controller_intent`) as the `CombatIntent` a pad
    /// adapter would emit.
    Pad(PadScript),
}

impl Controller {
    /// Name as the report prints it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::Pad(PadScript::Rush) => "pad-rush",
            Self::Pad(PadScript::Stand) => "pad-stand",
        }
    }
}

/// A scripted pad pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadScript {
    /// Run at the other fighter and press light attack whenever it is in
    /// reach. Never swaps, never dodges.
    Rush,
    /// Stand still facing the other fighter and press nothing: a target
    /// dummy.
    Stand,
}

/// The ground the duel is fought on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Flat and boundless: nothing blocks sight or movement but the
    /// fighters. A reference only: two shooters back off to their longest
    /// range and dodge every bolt, so AI pairs stall.
    Open,
    /// A walled square room [`ROOM_HALF`] units from the centre, about the
    /// size of the disc duel rooms (their fighters keep within roughly
    /// 500 by 600 units).
    Room,
    /// [`Layout::Room`] with six square pillars, point symmetric about the
    /// centre, so cover and peeking (and `DRY_CYCLES`) come into play.
    RoomPillars,
}

/// Half the side of the [`Layout::Room`] square, units.
pub const ROOM_HALF: i32 = 288;

impl Layout {
    /// Name as the report prints it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Room => "room",
            Self::RoomPillars => "pillars",
        }
    }

    /// Every layout, by name.
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Open, Self::Room, Self::RoomPillars]
            .into_iter()
            .find(|l| l.name() == name)
    }

    fn blockers(self) -> Vec<CharacterCollisionAabb> {
        let block = |x0: i32, z0: i32, x1: i32, z1: i32| {
            CharacterCollisionAabb::new(RoomPoint::new(x0, -1024, z0), RoomPoint::new(x1, 4096, z1))
        };
        let mut boxes = Vec::new();
        if self == Self::Open {
            return boxes;
        }
        let (h, t) = (ROOM_HALF, 64);
        boxes.push(block(-h - t, -h - t, h + t, -h));
        boxes.push(block(-h - t, h, h + t, h + t));
        boxes.push(block(-h - t, -h, -h, h));
        boxes.push(block(h, -h, h + t, h));
        if self == Self::RoomPillars {
            let p = 20;
            for (x, z) in [
                (0, 120),
                (0, -120),
                (170, 150),
                (-170, -150),
                (170, -150),
                (-170, 150),
            ] {
                boxes.push(block(x - p, z - p, x + p, z + p));
            }
        }
        boxes
    }
}

/// A fighter ready to run: its spec and its record, homed at the centre of
/// the arena (each fight places the body at its start).
#[derive(Clone, Debug)]
pub struct Fighter {
    /// The fighter's data.
    pub spec: FighterSpec,
    /// Its record, home at the arena centre, linking body 0 of `bodies`.
    record: &'static [LevelGameEntityRecord],
    /// Its body, as its side's one Character.
    bodies: &'static [LevelCharacterRecord],
}

impl Fighter {
    /// `spec` ready to fight.
    pub fn new(spec: FighterSpec) -> Self {
        let mut record = spec.record;
        record.room = RoomIndex::ZERO;
        record.x = 0;
        record.y = 0;
        record.z = 0;
        record.yaw = 0;
        record.patrol_x = 0;
        record.patrol_y = 0;
        record.patrol_z = 0;
        record.character = psx_level::CharacterIndex(0);
        let record: &'static [LevelGameEntityRecord] = Box::leak(Box::new([record]));
        let bodies: &'static [LevelCharacterRecord] = Box::leak(Box::new([spec.body]));
        Self {
            spec,
            record,
            bodies,
        }
    }
}

/// Where side `side` (0 A, 1 B) starts, and its yaw, for a fight whose two
/// starts lie `distance` apart along `bearing` (PSX angle units) through the
/// centre, each facing the other.
pub fn start(side: usize, distance: i32, bearing: u16) -> ([i32; 3], i16) {
    let axis = Angle::from_q12(bearing);
    let half = distance / 2;
    let sign = if side == 0 { -1 } else { 1 };
    let position = [
        sign * axis.sin().mul_i32(half),
        0,
        sign * axis.cos().mul_i32(half),
    ];
    let facing = if side == 0 {
        bearing
    } else {
        bearing.wrapping_add(2048) & 4095
    };
    (position, facing as i16)
}

/// The start bearing of duel seed `seed`: spread over the circle, so the
/// integer rounding of positions along one direction (which is not the
/// mirror of the rounding along the opposite one) does not favour a side
/// across a sweep.
pub const fn bearing_of(seed: u32) -> u16 {
    (seed.wrapping_mul(0x9e37_79b1) >> 20) as u16 & 4095
}

/// How a fight ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Side B died.
    AWon,
    /// Side A died.
    BWon,
    /// Both died on the same tick.
    DoubleKo,
    /// The time limit ran out.
    Timeout,
    /// Nobody took damage for [`STALL_TICKS`].
    Stalled,
}

impl Outcome {
    /// The disc's `duel:end` result code (1 enemy (B) dead, 2 player (A)
    /// dead, 3 double KO, 5 timeout, 6 stall).
    pub const fn code(self) -> u32 {
        match self {
            Self::AWon => 1,
            Self::BWon => 2,
            Self::DoubleKo => 3,
            Self::Timeout => 5,
            Self::Stalled => 6,
        }
    }

    /// The disc report's name, side A in the player's column.
    pub const fn disc_name(self) -> &'static str {
        match self {
            Self::AWon => "player_won",
            Self::BWon => "enemy_won",
            Self::DoubleKo => "double_ko",
            Self::Timeout => "timeout",
            Self::Stalled => "stalled_no_damage",
        }
    }
}

/// What one side did in a fight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SideTally {
    /// Health lost.
    pub damage_taken: u32,
    /// Hits that cost health.
    pub damage_events: u32,
    /// Hits this side parried.
    pub parries: u32,
    /// Crit strikes and crit volleys this side landed or released.
    pub crits: u32,
    /// Times this side was knocked down.
    pub knockdowns: u32,
    /// Times this side was staggered (a poise break that is not a
    /// knockdown, a parry's riposte included).
    pub staggers: u32,
    /// Swaps this side's controller made on purpose to parry.
    pub parry_swaps: u32,
    /// Hits this side took under armour without breaking.
    pub armour_absorbs: u32,
    /// Trades: heavy hits this side landed with an attack that had absorbed
    /// a hit under armour.
    pub trades: u32,
    /// Projectile evade decisions (ranged-exchange mode 4 entries, the
    /// disc report's `projectile_evade_attempts`).
    pub evades: u32,
    /// Stance swaps.
    pub swaps: u32,
    /// Bolts released.
    pub shots: u32,
    /// Attacks started (windups).
    pub attacks: u32,
}

/// One fight's result.
#[derive(Clone, Debug)]
pub struct DuelResult {
    /// How it ended.
    pub outcome: Outcome,
    /// The tick it ended on.
    pub end_tick: u32,
    /// Side A, side B.
    pub sides: [SideTally; 2],
    /// The fight's `duel:` lines in the disc log format, when asked for.
    pub log: String,
}

/// One fight's setup.
#[derive(Clone, Copy, Debug)]
pub struct Duel<'a> {
    /// The two fighters, side A then side B.
    pub fighters: [&'a Fighter; 2],
    /// Who drives each side.
    pub controllers: [Controller; 2],
    /// Each side's random seed (the tactical controller's stream).
    pub seeds: [u32; 2],
    /// Start distance.
    pub distance: i32,
    /// Start bearing ([`start`]).
    pub bearing: u16,
    /// The ground.
    pub layout: Layout,
    /// Keep the `duel:` log.
    pub log: bool,
}

/// One side while a fight runs.
struct Side {
    spec: FighterSpec,
    records: &'static [LevelGameEntityRecord],
    entities: Box<Entities>,
    attacks: DeferredGameEntityAttacks<1>,
    /// Bolts in flight toward this side.
    incoming: CombatProjectiles<8>,
    controller: Controller,
    moved: bool,
    last_position: [i32; 3],
    last_state: GameEntityState,
    last_stance: VitalityChannelId,
    last_health: u16,
    last_role: u8,
    attack_start: u32,
    /// The start tick of the attack during which this side last absorbed a
    /// hit under armour.
    absorbed_in: Option<u32>,
    tally: SideTally,
    /// Its pose this tick and the tick before.
    poses: crate::pose::Poses,
}

/// Side `j` as side `i` perceives it at the start of a think.
#[derive(Clone, Copy)]
struct Seen {
    position: [i32; 3],
    radius: i32,
    height: i32,
    combatant: Combatant,
    swap_invulnerable_ticks: u8,
    parry_ticks: u16,
    dodge: bool,
    stance: VitalityChannelId,
    attack_read: u8,
    swing_ticks: Option<u16>,
    moved: bool,
}

impl Side {
    fn record(&self) -> &'static LevelGameEntityRecord {
        &self.records[0]
    }

    /// Its body: the Character its record links.
    fn body(&self) -> &'static LevelCharacterRecord {
        self.entities.body(self.record())
    }

    fn seen(&self) -> Seen {
        let r = self.body();
        let defender = self
            .entities
            .defender(self.records, 0)
            .expect("the fighter's slot is live");
        let melee = self.entities.attack_kind(0) != 2;
        // The swing as a watcher reads it: its authored contact window on
        // the swing's clock (the disc reads the same frames off the posed
        // clip), else the record's contact ticks.
        let swing = self
            .entities
            .committed_swing(self.records, 0)
            .filter(|_| melee)
            .map(|(action, first, last)| {
                let phase = self.entities.clip_for_state(self.records, 0).phase_ticks;
                let (first, last) = self.spec.contact_window(action).unwrap_or((first, last));
                (phase, first, last)
            });
        let swing_ticks = swing.and_then(|(phase, first, last)| {
            ticks_to_contact(first, last, u32::from(phase) << 12, 1 << 12)
        });
        // The visible attack phase: 1 tell, 2 swing, 3 recovery.
        let attack_read = match (self.entities.attack_phase(0), swing) {
            (Some(AttackPhase::Windup | AttackPhase::Active), Some((phase, first, last))) => {
                if phase < first {
                    1
                } else if phase <= last {
                    2
                } else {
                    3
                }
            }
            (Some(AttackPhase::Recover), _) if melee => 3,
            _ => 0,
        };
        Seen {
            position: self.entities.position(0),
            radius: i32::from(r.radius),
            height: i32::from(r.height),
            combatant: *defender.combatant,
            swap_invulnerable_ticks: defender.swap_invulnerable_ticks,
            parry_ticks: defender.parry_ticks,
            dodge: defender.action.dodge,
            stance: self.entities.stance(0),
            attack_read,
            swing_ticks,
            moved: self.moved,
        }
    }

    /// What a bolt strikes, the disc's one rule
    /// (`combat::projectile_hurtboxes`): every authored hurtbox on this
    /// tick's pose, else the body capsule.
    fn hurtboxes(&self, push: impl FnMut(WorldCombatCapsule)) {
        let r = self.body();
        projectile_hurtboxes(
            self.spec.rig.as_ref().map_or(&[][..], |rig| rig.capsules),
            self.poses.current(),
            body_hurt_capsule(
                self.entities.position(0),
                i32::from(r.radius),
                i32::from(r.height),
            ),
            push,
        );
    }
}

/// The owner's world for one fighter's think: the other body and the
/// pillars block it, the pillars block sight, and a pad's intent possesses
/// it.
struct ArenaWorld<'a> {
    other: [CharacterCollisionCylinder; 1],
    pillars: &'a [CharacterCollisionAabb],
    intent: Option<CombatIntent>,
}

impl FighterWorld for ArenaWorld<'_> {
    fn move_body(
        &mut self,
        _entity: usize,
        _room: RoomIndex,
        motor: &mut CharacterMotorState,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta: u16,
    ) -> CharacterMotorFrame {
        let mut space = OpenSpace;
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut space, &self.other, self.pillars);
        motor
            .update_vblanks_with_trace_provider(&mut provider, input, config, delta)
            .expect("arena traces stay in range")
    }

    fn push_body(
        &mut self,
        _entity: usize,
        _room: RoomIndex,
        position: [i32; 3],
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
    ) -> [i32; 3] {
        let to = [position[0] + dx, position[1], position[2] + dz];
        let other = self.other[0];
        let gap = i64::from(radius + other.radius);
        let ox = i64::from(to[0] - other.position.x);
        let oz = i64::from(to[2] - other.position.z);
        let into_body = ox * ox + oz * oz < gap * gap;
        let into_pillar = self.pillars.iter().any(|b| {
            to[0] + radius > b.min.x
                && to[0] - radius < b.max.x
                && to[2] + radius > b.min.z
                && to[2] - radius < b.max.z
                && to[1] + height > b.min.y
                && to[1] < b.max.y
        });
        if into_body || into_pillar {
            position
        } else {
            to
        }
    }

    fn speed_scale(&self) -> (i32, i32) {
        SPEED_SCALE
    }

    fn line_of_sight(&mut self, _room: RoomIndex, from: [i32; 3], to: [i32; 3]) -> bool {
        !self
            .pillars
            .iter()
            .any(|b| segment_enters(from, to, b).is_some())
    }

    fn controller_intent(&mut self, _entity: usize) -> Option<CombatIntent> {
        self.intent
    }
}

/// Where segment `from`-`to` first enters box `b`, as a fraction of the
/// segment in 1/65536ths. None when it misses.
fn segment_enters(from: [i32; 3], to: [i32; 3], b: &CharacterCollisionAabb) -> Option<i64> {
    let lo = [b.min.x, b.min.y, b.min.z];
    let hi = [b.max.x, b.max.y, b.max.z];
    let (mut enter, mut leave) = (0i64, 65_536i64);
    for axis in 0..3 {
        let p = i64::from(from[axis]);
        let d = i64::from(to[axis]) - p;
        let (l, h) = (i64::from(lo[axis]), i64::from(hi[axis]));
        if d == 0 {
            if p < l || p > h {
                return None;
            }
            continue;
        }
        let mut t0 = (l - p) * 65_536 / d;
        let mut t1 = (h - p) * 65_536 / d;
        if t0 > t1 {
            std::mem::swap(&mut t0, &mut t1);
        }
        enter = enter.max(t0);
        leave = leave.min(t1);
        if enter > leave {
            return None;
        }
    }
    Some(enter)
}

/// Bolts stop at the pillars.
struct PillarTracer<'a> {
    pillars: &'a [CharacterCollisionAabb],
}

impl ProjectileWorldTracer for PillarTracer<'_> {
    fn trace_projectile(
        &mut self,
        _room: RoomIndex,
        start: [i32; 3],
        end: [i32; 3],
        _radius: u16,
    ) -> ProjectileWorldTrace {
        let first = self
            .pillars
            .iter()
            .filter_map(|b| segment_enters(start, end, b))
            .min();
        match first {
            None => ProjectileWorldTrace::Clear { end },
            Some(t) => {
                let at = |a: usize| {
                    (i64::from(start[a]) + (i64::from(end[a]) - i64::from(start[a])) * t / 65_536)
                        as i32
                };
                ProjectileWorldTrace::Hit {
                    end: [at(0), at(1), at(2)],
                }
            }
        }
    }
}

/// The scripted pad's intent for a fighter at `me` facing anything, the
/// other at `them`, with melee reach `reach`.
fn pad_intent(script: PadScript, me: [i32; 3], them: [i32; 3], reach: i32) -> CombatIntent {
    match script {
        PadScript::Stand => CombatIntent {
            face: Some(them),
            ..CombatIntent::IDLE
        },
        PadScript::Rush => {
            let dx = i64::from(them[0] - me[0]);
            let dz = i64::from(them[2] - me[2]);
            let length = ((dx * dx + dz * dz) as f64).sqrt().max(1.0) as i64;
            let mut intent = CombatIntent {
                face: Some(them),
                ..CombatIntent::IDLE
            };
            if length > i64::from(reach) {
                intent.movement = CombatMove::Move {
                    x: (dx * 4096 / length) as i16,
                    z: (dz * 4096 / length) as i16,
                    forward: 0,
                };
            } else {
                intent.pressed = CombatActions::of(CombatAction::LightAttack);
            }
            intent
        }
    }
}

/// Whether nothing blocks a melee contact between two fighters' chests.
fn clear_line(pillars: &[CharacterCollisionAabb], a: [i32; 3], b: [i32; 3]) -> bool {
    let lift = |p: [i32; 3]| [p[0], p[1] + 32, p[2]];
    !pillars
        .iter()
        .any(|box_| segment_enters(lift(a), lift(b), box_).is_some())
}

/// The impact colour that marks a bolt of a crit volley (the disc tags its
/// crit bolts by their impact colour too).
const CRIT_BOLT_MARK: [u8; 3] = [255, 0, 255];

/// A pending hit on `victim`, latched this tick, landed after both sides'
/// contacts are known.
#[derive(Clone, Copy)]
struct PendingHit {
    attacker: usize,
    victim: usize,
    kind: HitKind,
    channel: VitalityChannelId,
    damage: u16,
    poise: u16,
    heavy: bool,
    /// Whether the victim's windup was open to a bolt interrupt before
    /// any hit this tick.
    opening: bool,
    /// A crit swing or a bolt of a crit volley.
    crit: bool,
}

fn log_line(log: &mut String, label: &str, values: &[u32]) {
    log.push_str(label);
    for v in values {
        let _ = write!(log, " {v:08x}");
    }
    log.push('\n');
}

/// Run one fight.
// Each step reads side `i` and the other side `1 - i` by index.
#[allow(clippy::needless_range_loop)]
pub fn run(duel: &Duel<'_>) -> DuelResult {
    let pillars = duel.layout.blockers();
    let mut sides: Vec<Side> = (0..2)
        .map(|n| {
            let fighter = duel.fighters[n];
            let records = fighter.record;
            let mut entities = Box::new(Entities::EMPTY);
            entities.set_characters(fighter.bodies);
            entities.spawn_from_records(records);
            let (at, yaw) = start(n, duel.distance, duel.bearing);
            let placed = entities.place_training_actor(records, 0, at, yaw);
            assert!(placed, "a duel fighter is a training record");
            entities.set_spatial_active_mask(u64::MAX);
            entities.seed_training_actor(records, 0, duel.seeds[n]);
            // The game times its entities' attacks after spawning them.
            entities.time_attacks(records, |_, action, _, _, _| fighter.spec.timing(action));
            entities.time_reactions(records, |_, reaction, _| match reaction {
                Reaction::Flinch => fighter.spec.flinch_ticks,
                _ => fighter.spec.stagger_ticks,
            });
            let position = entities.position(0);
            Side {
                spec: fighter.spec.clone(),
                records,
                last_state: entities.state(0),
                last_stance: entities.stance(0),
                last_health: entities.health(0),
                entities,
                attacks: DeferredGameEntityAttacks::EMPTY,
                incoming: CombatProjectiles::new(),
                controller: duel.controllers[n],
                moved: false,
                last_position: position,
                last_role: 0,
                attack_start: 0,
                absorbed_in: None,
                tally: SideTally::default(),
                poses: crate::pose::Poses::default(),
            }
        })
        .collect();
    let mut log = String::new();
    if duel.log {
        log_line(&mut log, "duel:start", &[duel.seeds[0], 1, 1]);
    }
    let mut last_progress = 0u32;
    let mut last_sample: Option<[u32; 21]> = None;
    let mut tick = 0u32;
    let outcome = loop {
        // Think (30 Hz): both from snapshots taken before either moves.
        if tick.is_multiple_of(u32::from(THINK_DELTA)) {
            let seen = [sides[0].seen(), sides[1].seen()];
            for i in 0..2 {
                let j = 1 - i;
                let other = seen[j];
                let side = &mut sides[i];
                let records = side.records;
                side.entities.observe_projectiles(records, &side.incoming);
                let intent = match side.controller {
                    Controller::Ai => None,
                    Controller::Pad(script) => {
                        // The AI's swing distance (its blade's), a step inside.
                        let reach = melee_swing_reach(
                            side.record().melee_reach,
                            i32::from(side.body().radius),
                            other.radius,
                        ) + 8;
                        Some(pad_intent(
                            script,
                            side.entities.position(0),
                            other.position,
                            reach,
                        ))
                    }
                };
                let mut world = ArenaWorld {
                    other: [CharacterCollisionCylinder::new(
                        RoomPoint::new(other.position[0], other.position[1], other.position[2]),
                        other.radius,
                        other.height,
                    )],
                    pillars: &pillars,
                    intent,
                };
                let combatant = other.combatant;
                let input = GameEntityTickInput {
                    target: CombatTarget {
                        position: other.position,
                        radius: other.radius,
                        height: other.height,
                        noise_radius: if other.moved { other.height * 2 } else { 0 },
                        defender: Defender {
                            combatant: &combatant,
                            swap_invulnerable_ticks: other.swap_invulnerable_ticks,
                            parry_ticks: other.parry_ticks,
                            action: psx_game_runtime::combatant::ActionProtection {
                                authored: None,
                                dodge: other.dodge,
                            },
                        },
                        stance: Some(other.stance),
                        attack_read: other.attack_read,
                        swing_ticks: other.swing_ticks,
                    },
                };
                side.entities.tick_delta_deferred(
                    records,
                    input,
                    &mut world,
                    THINK_DELTA,
                    &mut side.attacks,
                );
            }
        }
        // Every fighter's swap clock, once per tick after any swap.
        for side in &mut sides {
            side.entities.advance_stances(1);
        }
        // Freeze each body's pose for this tick, as the disc does before
        // combat resolves.
        for side in &mut sides {
            let Some(rig) = side.spec.rig.clone() else {
                continue;
            };
            let now = psx_engine::SimTick::from_u32(tick);
            side.entities
                .update_animation(side.records, now, |_, clip| rig.clip_ticks(clip));
            let attack = side
                .attacks
                .get(0)
                .map(|attack| (attack.action(), attack.clip()));
            let pose = rig.pose(&side.entities, &side.records[0], attack, now);
            side.poses.advance(pose);
        }

        // Contacts. Melee first, both directions latched before either lands.
        let mut pending: Vec<PendingHit> = Vec::new();
        for i in 0..2 {
            let j = 1 - i;
            let Some(attack) = sides[i].attacks.get(0) else {
                continue;
            };
            if !sides[i].entities.deferred_attack_can_connect(attack) {
                continue;
            }
            if attack.is_ranged() {
                release_bolts(&mut sides, i, attack, duel.log, tick, &mut log);
                continue;
            }
            let (victim_position, victim_radius) = (
                sides[j].entities.position(0),
                i32::from(sides[j].body().radius),
            );
            if sides[j].entities.passes_through(sides[j].records, 0) {
                continue;
            }
            let records = sides[i].records;
            // The swing's contact, the disc's one melee contact path: the
            // attacker's hitboxes active on its pose, swept from the tick
            // before, against the defender's hurtboxes on its pose.
            let (Some(attacker_rig), Some(defender_rig), Some(pose)) = (
                sides[i].spec.rig.as_ref(),
                sides[j].spec.rig.as_ref(),
                sides[i].poses.current(),
            ) else {
                continue;
            };
            let strike = ActiveStrike::sample(
                attacker_rig.capsules,
                attack.action(),
                pose,
                sides[i].poses.previous(),
            );
            let spent = sides[i].entities.spent_windows(0, 1);
            let Some(contact) = strike.contact(
                attacker_rig.capsules,
                defender_rig.capsules,
                sides[j].poses.current(),
                spent,
            ) else {
                continue;
            };
            let _ = victim_radius;
            if !clear_line(&pillars, attack.position(), victim_position) {
                continue;
            }
            // The one swing latch: the other fighter is defender 1.
            if !sides[i].entities.latch_strike(0, 1, contact.window) {
                continue;
            }
            let (damage, poise) =
                sides[i]
                    .entities
                    .crit_scaled(records, 0, contact.damage, contact.poise_damage);
            // The swing that reached its target uses up the crit, parried
            // or not.
            let crit = sides[i].entities.spend_crit(0);
            if crit {
                sides[i].tally.crits += 1;
                if duel.log {
                    log_line(&mut log, "duel:event", &[tick, i as u32, 2]);
                }
            }
            pending.push(PendingHit {
                attacker: i,
                victim: j,
                kind: HitKind::Melee,
                channel: sides[i].entities.stance(0),
                damage,
                poise,
                heavy: sides[i].entities.attack_kind(0) == 1,
                opening: false,
                crit,
            });
        }
        // Bolts: each side's incoming pool against its own body.
        for j in 0..2 {
            let i = 1 - j;
            let mut targets: Vec<ProjectileTarget> = Vec::new();
            if sides[j].entities.state(0) != GameEntityState::Dead
                && !sides[j].entities.passes_through(sides[j].records, 0)
            {
                sides[j].hurtboxes(|hurtbox| {
                    targets.push(ProjectileTarget {
                        target: 0,
                        team: CombatTeam::Enemy,
                        room: RoomIndex::ZERO,
                        hurtbox,
                    })
                });
            }
            let mut impacts = ProjectileImpacts::<8>::new();
            let opening = sides[j].entities.shot_opening(sides[j].records, 0);
            let mut tracer = PillarTracer { pillars: &pillars };
            let _ = sides[j].incoming.tick(&targets, &mut tracer, &mut impacts);
            for impact in impacts.as_slice() {
                if impact.kind != (ProjectileImpactKind::Target { target: 0 }) {
                    continue;
                }
                pending.push(PendingHit {
                    attacker: i,
                    victim: j,
                    kind: HitKind::Projectile,
                    channel: match impact.damage_channel {
                        ProjectileDamageChannel::Horizon => VitalityChannelId::One,
                        ProjectileDamageChannel::Zenith => VitalityChannelId::Two,
                    },
                    damage: impact.damage,
                    poise: impact.poise_damage,
                    heavy: false,
                    opening,
                    crit: impact.visual.impact_rgb == CRIT_BOLT_MARK,
                });
            }
        }
        for hit in pending {
            land(&mut sides, hit, duel.log, tick, &mut log);
        }

        // Observe.
        for (n, side) in sides.iter_mut().enumerate() {
            let state = side.entities.state(0);
            let position = side.entities.position(0);
            side.moved = position != side.last_position;
            side.last_position = position;
            if state != side.last_state {
                if state == GameEntityState::Staggered {
                    side.tally.staggers += 1;
                }
                if state == GameEntityState::KnockedDown {
                    side.tally.knockdowns += 1;
                    if duel.log {
                        log_line(&mut log, "duel:event", &[tick, n as u32, 3]);
                    }
                }
                if state == GameEntityState::Attack {
                    side.tally.attacks += 1;
                    side.attack_start = tick;
                }
                side.last_state = state;
            }
            let stance = side.entities.stance(0);
            if stance != side.last_stance {
                side.tally.swaps += 1;
                if side.entities.tactical_snapshot(0).stance_reason
                    == psx_game_runtime::entities::STANCE_REASON_PARRY
                {
                    side.tally.parry_swaps += 1;
                }
                side.last_stance = stance;
            }
            let health = side.entities.health(0);
            if health < side.last_health {
                side.tally.damage_taken += u32::from(side.last_health - health);
                side.tally.damage_events += 1;
                last_progress = tick;
            }
            side.last_health = health;
            let role = side.entities.combat_role(0);
            if role != side.last_role {
                if role == 4 {
                    side.tally.evades += 1;
                }
                if duel.log {
                    log_line(&mut log, "duel:tactic", &[tick, n as u32, u32::from(role)]);
                }
                side.last_role = role;
            }
        }
        if duel.log {
            let sample = sample_row(&sides, tick);
            let changed = last_sample.is_none_or(|old| {
                old[5..7] != sample[5..7] || old[7..11] != sample[7..11] || old[15] != sample[15]
            });
            if tick.is_multiple_of(60) || changed {
                log_line(&mut log, "duel:sample", &sample);
            }
            last_sample = Some(sample);
        }
        let dead = [
            sides[0].entities.state(0) == GameEntityState::Dead,
            sides[1].entities.state(0) == GameEntityState::Dead,
        ];
        let end = match dead {
            [true, true] => Some(Outcome::DoubleKo),
            [false, true] => Some(Outcome::AWon),
            [true, false] => Some(Outcome::BWon),
            _ if tick >= TIME_LIMIT_TICKS => Some(Outcome::Timeout),
            _ if tick.saturating_sub(last_progress) >= STALL_TICKS => Some(Outcome::Stalled),
            _ => None,
        };
        if let Some(outcome) = end {
            break outcome;
        }
        tick += 1;
    };
    if duel.log {
        log_line(&mut log, "duel:end", &[tick, outcome.code()]);
    }
    DuelResult {
        outcome,
        end_tick: tick,
        sides: [sides[0].tally, sides[1].tally],
        log,
    }
}

/// Release every emitter of side `i`'s ranged token not yet fired, while it
/// can pay: the disc's `release_projectiles` rule on this tick's pose (each
/// emitter from its own muzzle, once the clip frame reaches its window).
fn release_bolts(
    sides: &mut [Side],
    i: usize,
    attack: psx_game_runtime::entities::DeferredGameEntityAttack,
    logging: bool,
    tick: u32,
    log: &mut String,
) {
    let j = 1 - i;
    let records = sides[i].records;
    let Some(target) = sides[i].entities.ranged_target(0) else {
        return;
    };
    let crit = sides[i].entities.attack_is_crit(0);
    let mut released_crit = false;
    // The disc's one release rule (`release_projectiles`): each authored
    // emitter on this tick's pose fires from its own muzzle once the clip
    // frame reaches its window.
    let capsules = sides[i]
        .spec
        .rig
        .as_ref()
        .map_or(&[][..], |rig| rig.capsules);
    let Some(pose) = sides[i].poses.current().copied() else {
        return;
    };
    let frame = (pose.phase_q12() >> 12).min(u32::from(u16::MAX)) as u16;
    while let Some(mask) = sides[i].entities.deferred_projectile_release_mask(attack) {
        let Some((emitter, release)) = authored_projectile_release_pending_at_frame(
            capsules,
            attack.action(),
            Some(&pose),
            mask,
            frame,
        ) else {
            break;
        };
        if !sides[i].entities.can_fire_energy(&records[0], 0) {
            break;
        }
        let (damage, poise) = if crit {
            sides[i]
                .entities
                .crit_scaled(records, 0, release.damage, release.poise_damage)
        } else {
            (release.damage, release.poise_damage)
        };
        let spawn = ProjectileSpawn {
            position: release.position,
            velocity: [0; 3],
            radius: release.radius,
            damage,
            poise_damage: poise,
            lifetime_ticks: release.lifetime_ticks,
            room: RoomIndex::ZERO,
            team: CombatTeam::Player,
            // The pool holds only bolts at this side, its one target is id
            // 0, and a bolt never hits its owner: any other id.
            owner: 1,
            tint_rgb: [0; 3],
            damage_channel: release.damage_channel,
            visual: if crit {
                ProjectileVisualStyle {
                    impact_rgb: CRIT_BOLT_MARK,
                    ..ProjectileVisualStyle::EMPTY
                }
            } else {
                ProjectileVisualStyle::EMPTY
            },
        };
        if sides[j]
            .incoming
            .spawn_toward(spawn, target, release.speed)
            .is_err()
        {
            break;
        }
        sides[i].entities.spend_shot_energy(&records[0], 0);
        let _ = sides[i]
            .entities
            .commit_deferred_projectile(attack, emitter);
        sides[i].tally.shots += 1;
        released_crit |= crit;
        if logging {
            log_line(
                log,
                "duel:shot",
                &[tick, i as u32, u32::from(sides[i].entities.energy(0))],
            );
        }
    }
    if released_crit {
        sides[i].entities.spend_crit(0);
        sides[i].tally.crits += 1;
        if logging {
            log_line(log, "duel:event", &[tick, i as u32, 2]);
        }
    }
}

/// Land one latched hit on its victim through the victim's own hit path; a
/// parry staggers the attacker through the attacker's.
fn land(sides: &mut [Side], hit: PendingHit, logging: bool, tick: u32, log: &mut String) {
    let (i, j) = (hit.attacker, hit.victim);
    let victim_records = sides[j].records;
    let attacker_records = sides[i].records;
    let armoured = sides[j].entities.armoured(victim_records, 0);
    // The victim's side, then the attacker's: the one landing.
    let outcome = match hit.kind {
        HitKind::Projectile => sides[j].entities.apply_projectile_hit(
            victim_records,
            0,
            hit.channel,
            hit.damage,
            hit.poise,
            hit.crit,
        ),
        _ => sides[j].entities.receive_hit(
            victim_records,
            0,
            Strike {
                // Not a slot of the victim's table: the attacker's table
                // answers for it below.
                attacker: CombatantId::PLAYER,
                kind: hit.kind,
                channel: hit.channel,
                damage: hit.damage,
                poise: hit.poise,
                crit: hit.crit,
            },
        ),
    };
    sides[i].entities.answer_strike(
        attacker_records,
        0,
        CombatantId::PLAYER,
        hit.kind,
        hit.heavy,
        outcome,
        hit.opening,
    );
    if outcome.parried {
        sides[j].tally.parries += 1;
        if logging {
            log_line(log, "duel:event", &[tick, j as u32, 1]);
        }
        return;
    }
    if armoured && outcome.connected && !outcome.staggered {
        sides[j].tally.armour_absorbs += 1;
        sides[j].absorbed_in = Some(sides[j].attack_start);
    }
    if hit.heavy && outcome.connected && sides[i].absorbed_in == Some(sides[i].attack_start) {
        sides[i].tally.trades += 1;
    }
}

/// One `duel:sample` row in the disc's 21-column layout, side A in the
/// player's columns and side B in the enemy's.
fn sample_row(sides: &[Side], tick: u32) -> [u32; 21] {
    let (a, b) = (&sides[0], &sides[1]);
    let pa = a.entities.position(0);
    let pb = b.entities.position(0);
    let action = |s: &Side| -> u32 {
        match s.entities.attack_phase(0) {
            Some(AttackPhase::Windup | AttackPhase::Active) => match s.entities.attack_kind(0) {
                0 => CharacterAnimationAction::LightAttack.to_index() as u32,
                1 => CharacterAnimationAction::HeavyAttack.to_index() as u32,
                _ => CharacterAnimationAction::RangedAttack.to_index() as u32,
            },
            _ => 0,
        }
    };
    let ta = a.entities.tactical_snapshot(0);
    let tb = b.entities.tactical_snapshot(0);
    let stamina = a.entities.motor(0).map_or(0, |m| m.stamina_q12() as u32);
    [
        tick,
        pa[0] as u32,
        pa[2] as u32,
        pb[0] as u32,
        pb[2] as u32,
        u32::from(a.entities.health(0)),
        u32::from(b.entities.health(0)),
        a.entities.stance(0).index() as u32,
        b.entities.stance(0).index() as u32,
        action(a),
        b.entities.state(0) as u32,
        0,
        u32::from(ta.stance_reason),
        stamina,
        u32::from(tb.generation),
        a.attack_start,
        tb.goal as u32,
        u32::from(tb.stance_reason),
        u32::from(b.entities.attack_kind(0)),
        u32::from(a.entities.energy(0)),
        u32::from(b.entities.energy(0)),
    ]
}
