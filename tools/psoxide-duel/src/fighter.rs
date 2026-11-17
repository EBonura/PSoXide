//! One fighter as the arena runs it: a cooked entity record (body, motion,
//! combat stats, kits, AI persona) and what its authored attacks deal.

use psx_level::{combat_capsule_flags, CharacterAnimationAction, LevelGameEntityRecord};
use psxed_project::playtest::PlaytestCombatCapsule;

/// Damage and poise of one strike, as its authored hitbox deals them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrikeData {
    /// Health damage.
    pub damage: u16,
    /// Poise damage.
    pub poise: u16,
}

/// One authored projectile emitter of an attack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Emitter {
    /// The `CharacterAnimationAction` index that fires it.
    pub action: u8,
    /// Index of the emitter among the fighter's capsules: the release latch
    /// bit the entity table keeps per attack.
    pub latch: u8,
    /// Units per tick.
    pub speed: u16,
    /// Bolt radius.
    pub radius: u16,
    /// Health damage.
    pub damage: u16,
    /// Poise damage.
    pub poise: u16,
    /// Ticks before the bolt expires.
    pub lifetime_ticks: u16,
    /// Cooked damage channel (`psx_level::projectile_damage_channel`).
    pub channel: u8,
    /// Tick into the attack clip at which it releases (its first active
    /// frame through the clip's clock); 0 when unknown.
    pub release_tick: u16,
}

/// A fighter: everything the arena needs about one character. Whoever it
/// is (Aletha or an enemy), it is an entity record from the same cook, so it
/// runs the same rules; only this data differs.
#[derive(Clone, Debug)]
pub struct FighterSpec {
    /// Display name (the Character resource).
    pub name: String,
    /// The cooked entity record. `combat_capsule_first` is meaningless here.
    pub record: LevelGameEntityRecord,
    /// Authored strike per action index, `None` where the character has no
    /// hitbox for it (the legacy contact then deals the record's touch
    /// damage, as on the disc).
    pub strikes: Vec<(u8, StrikeData)>,
    /// Authored projectile emitters.
    pub emitters: Vec<Emitter>,
    /// Authored contact window per action index: the ticks into the
    /// attack clip (from the start of the windup) over which its hitboxes
    /// are active, through the clip's clock. The disc reads the same window
    /// off the posed clip.
    pub contact: Vec<(u8, u16, u16)>,
    /// The cooked capsules the above came from.
    pub capsules: Vec<PlaytestCombatCapsule>,
    /// How its light, heavy and ranged attacks run, read off its clips by the
    /// guest's own rules (`GameEntities::time_attacks`); `None` keeps the
    /// record's ticks, as on the disc.
    pub timings: [Option<psx_game_runtime::fighter_action::ActionTiming>; 3],
}

impl FighterSpec {
    /// A fighter from its cooked `record` and its own cooked capsules.
    pub fn new(
        name: &str,
        record: LevelGameEntityRecord,
        capsules: &[PlaytestCombatCapsule],
    ) -> Self {
        let mut strikes: Vec<(u8, StrikeData)> = Vec::new();
        let mut emitters = Vec::new();
        for (latch, capsule) in capsules.iter().enumerate() {
            if capsule.flags & combat_capsule_flags::HITBOX != 0
                && !strikes.iter().any(|(action, _)| *action == capsule.action)
            {
                strikes.push((
                    capsule.action,
                    StrikeData {
                        damage: capsule.damage,
                        poise: capsule.poise_damage,
                    },
                ));
            }
            if capsule.flags & combat_capsule_flags::PROJECTILE_EMITTER != 0 && latch < 16 {
                emitters.push(Emitter {
                    action: capsule.action,
                    latch: latch as u8,
                    speed: capsule.projectile_speed,
                    radius: capsule.radius,
                    damage: capsule.damage,
                    poise: capsule.poise_damage,
                    lifetime_ticks: capsule.projectile_lifetime_ticks,
                    channel: capsule.projectile_damage_channel,
                    release_tick: 0,
                });
            }
        }
        Self {
            name: name.to_owned(),
            record,
            strikes,
            emitters,
            contact: Vec::new(),
            capsules: capsules.to_vec(),
            timings: [None; 3],
        }
    }

    /// What a melee swing of `action` deals: its authored hitbox, else the
    /// legacy contact's numbers (the record's touch damage, at least 20
    /// poise), exactly the disc's fallback.
    pub fn strike(&self, action: CharacterAnimationAction) -> StrikeData {
        let index = action.to_index() as u8;
        self.strikes
            .iter()
            .find(|(a, _)| *a == index)
            .map(|(_, strike)| *strike)
            .unwrap_or(StrikeData {
                damage: self.record.touch_damage,
                poise: self.record.touch_damage.max(20),
            })
    }

    /// The authored contact window of `action`, ticks into the swing.
    pub fn contact_window(&self, action: CharacterAnimationAction) -> Option<(u16, u16)> {
        let index = action.to_index() as u8;
        self.contact
            .iter()
            .find(|(a, _, _)| *a == index)
            .map(|(_, first, last)| (*first, *last))
    }

    /// The timing of the attack kind `action` names ([`Self::timings`]).
    pub fn timing(
        &self,
        action: CharacterAnimationAction,
    ) -> Option<psx_game_runtime::fighter_action::ActionTiming> {
        let index = action.to_index() as u8;
        let kind = if action == CharacterAnimationAction::HeavyAttack {
            1
        } else if index == self.record.ranged_attack_action {
            2
        } else {
            0
        };
        self.timings[kind]
    }

    /// The emitters `action` fires.
    pub fn emitters_of(&self, action: CharacterAnimationAction) -> impl Iterator<Item = &Emitter> {
        let index = action.to_index() as u8;
        self.emitters.iter().filter(move |e| e.action == index)
    }
}

/// A placeholder number nudged for a sensitivity run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placeholder {
    /// `CombatStats::parry_ticks`.
    ParryTicks,
    /// `CombatStats::parry_refund`.
    ParryRefund,
    /// `CombatStats::crit_window_ticks`.
    CritWindowTicks,
    /// `CombatStats::crit_damage_q12`.
    CritDamageQ12,
    /// `CombatStats::crit_poise_q12`.
    CritPoiseQ12,
    /// `CombatStats::knockdown_ticks`.
    KnockdownTicks,
    /// `CombatStats::poise`.
    Poise,
    /// `CombatStats::parry_swap_percent`.
    ParrySwapPercent,
    /// `ranged_tactics::DRY_CYCLES` (a code constant; the harness feature
    /// makes it a setting).
    DryCycles,
}

impl Placeholder {
    /// Every placeholder, in report order.
    pub const ALL: [Self; 9] = [
        Self::ParryTicks,
        Self::ParryRefund,
        Self::CritWindowTicks,
        Self::CritDamageQ12,
        Self::CritPoiseQ12,
        Self::KnockdownTicks,
        Self::Poise,
        Self::ParrySwapPercent,
        Self::DryCycles,
    ];

    /// Name as the report prints it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ParryTicks => "parry_ticks",
            Self::ParryRefund => "parry_refund",
            Self::CritWindowTicks => "crit_window_ticks",
            Self::CritDamageQ12 => "crit_damage_q12",
            Self::CritPoiseQ12 => "crit_poise_q12",
            Self::KnockdownTicks => "knockdown_ticks",
            Self::Poise => "poise",
            Self::ParrySwapPercent => "parry_swap_percent",
            Self::DryCycles => "dry_cycles",
        }
    }

    /// One step of this placeholder: small enough to stay a nudge, large
    /// enough to be more than rounding. Poise moves by a fifth of the
    /// fighter's own value (the fighters' poise differs by more than 2x).
    pub fn step(self, current: u16) -> u16 {
        match self {
            Self::ParryTicks => 2,
            Self::ParryRefund => 10,
            Self::CritWindowTicks => 20,
            Self::CritDamageQ12 | Self::CritPoiseQ12 => 1024,
            Self::KnockdownTicks => 20,
            Self::Poise => (current / 5).max(1),
            Self::ParrySwapPercent => 25,
            Self::DryCycles => 1,
        }
    }

    /// Nudge this placeholder on `record` by `direction` steps (-1 or +1).
    /// Returns the value set. `DryCycles` is not on the record (see
    /// [`crate::arena::set_dry_cycles`]); it returns the record unchanged.
    pub fn nudge(self, record: &mut LevelGameEntityRecord, direction: i32) -> u16 {
        let c = &mut record.combat;
        let field: &mut u16 = match self {
            Self::ParryTicks => &mut c.parry_ticks,
            Self::ParryRefund => &mut c.parry_refund,
            Self::CritWindowTicks => &mut c.crit_window_ticks,
            Self::CritDamageQ12 => &mut c.crit_damage_q12,
            Self::CritPoiseQ12 => &mut c.crit_poise_q12,
            Self::KnockdownTicks => &mut c.knockdown_ticks,
            Self::Poise => &mut c.poise,
            Self::ParrySwapPercent => {
                let step = i32::from(self.step(0));
                let value = (i32::from(c.parry_swap_percent) + step * direction).clamp(0, 100);
                c.parry_swap_percent = value as u8;
                return value as u16;
            }
            Self::DryCycles => return 0,
        };
        let step = i32::from(self.step(*field));
        let value = (i32::from(*field) + step * direction).clamp(0, i32::from(u16::MAX));
        *field = value as u16;
        value as u16
    }

    /// The value of this placeholder on `record`.
    pub fn value(self, record: &LevelGameEntityRecord) -> u16 {
        let c = &record.combat;
        match self {
            Self::ParryTicks => c.parry_ticks,
            Self::ParryRefund => c.parry_refund,
            Self::CritWindowTicks => c.crit_window_ticks,
            Self::CritDamageQ12 => c.crit_damage_q12,
            Self::CritPoiseQ12 => c.crit_poise_q12,
            Self::KnockdownTicks => c.knockdown_ticks,
            Self::Poise => c.poise,
            Self::ParrySwapPercent => u16::from(c.parry_swap_percent),
            Self::DryCycles => u16::from(psx_game_runtime::ranged_tactics::DRY_CYCLES),
        }
    }
}
