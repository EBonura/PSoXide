//! Combat state one fighter carries, the same type for the player and enemies.
//!
//! A [`Combatant`] is plain data plus thin rules over the shared leaf types:
//! health ([`DualVitality`]), posture ([`Poise`]), energy and poise-break
//! grace ([`CombatFlow`]) and the stance ([`CombatStance`]: side, swap
//! cooldown and swap clock). It holds no scene, no entity tables and no AI.
//! Where today's player and enemy rules still differ, the difference is a
//! parameter of the call (a cooldown, a capacity, whether break grace
//! applies) rather than a branch on who is fighting.
//!
//! Invulnerability is not stored: the swap window is read from the stance
//! clock with [`Combatant::in_swap_window`], so it can never go stale.
//!
//! Every hit, whatever dealt it, lands through [`Combatant::receive_hit`]. A
//! [`HitIn`] is the [`Strike`] (what the attacker sent) plus the defender's
//! [`HitRoute`] and [`PoiseRule`]. Today's player and enemy formulas still
//! differ, so they arrive as those two parameters, never as a side flag.

use crate::combat_flow::CombatFlow;
use crate::poise::Poise;
use crate::vitality::{
    CombatStance, CombatStanceConfig, DualVitality, StanceDamageOutcome, VitalityChannelId,
    VitalityPool, VITALITY_Q12_ONE,
};

/// Damage against the channel an enemy currently guards: a token 1 to 3
/// points whatever the attack's authored value, so a wrong-stance hit reads
/// at once as pointless and pushes the player to swap. The value is the
/// authored damage divided by [`GAME_ENTITY_GUARDED_DAMAGE_DIVISOR`] and
/// clamped into this range, so heavier swings still chip a little harder.
pub const GAME_ENTITY_GUARDED_DAMAGE_MIN: u16 = 1;

/// Upper bound of the guarded-channel chip damage.
pub const GAME_ENTITY_GUARDED_DAMAGE_MAX: u16 = 3;

/// Authored damage per point of guarded-channel chip damage.
pub const GAME_ENTITY_GUARDED_DAMAGE_DIVISOR: u16 = 16;

/// Damage against the channel an enemy leaves exposed (Q12 = 1.5x).
pub const GAME_ENTITY_OPPOSED_DAMAGE_Q12: u16 = 6144;

/// Who dealt a hit: an entity slot, the player, or the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatantId(pub u16);

impl CombatantId {
    /// The player character.
    pub const PLAYER: Self = Self(u16::MAX - 1);
    /// No fighter: hazards, liquids and other environment damage.
    pub const WORLD: Self = Self(u16::MAX);

    /// The entity in table slot `index`.
    pub const fn entity(index: usize) -> Self {
        Self(index as u16)
    }
}

/// What kind of source dealt a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    /// A melee swing, authored capsule or legacy arc.
    Melee,
    /// A projectile impact.
    Projectile,
    /// A hazard volume or a harmful liquid.
    Hazard,
    /// Anything else the world deals.
    Environment,
}

/// The attacker's half of a hit: who, how, on which channel and how hard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Strike {
    /// Who dealt it.
    pub attacker: CombatantId,
    /// What dealt it.
    pub kind: HitKind,
    /// The attack's vitality channel (stance colour).
    pub channel: VitalityChannelId,
    /// Health damage before the defender's routing scales it.
    pub damage: u16,
    /// Posture damage before the defender's routing scales it.
    pub poise: u16,
}

/// How a defender's guard scales a hit routed with [`HitRoute::Spill`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// The authored damage and poise, unscaled.
    Raw,
    /// The guarded channel takes a 1 to 3 point chip with its poise scaled
    /// by the same fraction; the exposed channel takes 1.5x and full poise.
    Chip,
    /// The guarded channel takes 1.0x, the exposed one 1.25x; poise as
    /// authored.
    Flow,
}

impl Guard {
    /// Health damage a hit of `damage` deals when it does (`guarded`) or
    /// does not match the defender's active channel. Zero stays zero.
    pub fn damage(self, guarded: bool, damage: u16) -> u16 {
        if damage == 0 {
            return 0;
        }
        match self {
            Self::Raw => damage,
            Self::Flow => (u32::from(damage) * if guarded { 4 } else { 5 } / 4).min(65535) as u16,
            Self::Chip if guarded => (damage / GAME_ENTITY_GUARDED_DAMAGE_DIVISOR).clamp(
                GAME_ENTITY_GUARDED_DAMAGE_MIN,
                GAME_ENTITY_GUARDED_DAMAGE_MAX,
            ),
            Self::Chip => ((u32::from(damage) * u32::from(GAME_ENTITY_OPPOSED_DAMAGE_Q12)) / 4096)
                .min(u32::from(u16::MAX)) as u16,
        }
    }

    /// Posture damage for the same hit: under [`Self::Chip`] a guarded hit
    /// keeps the fraction of its authored poise that [`Self::damage`] kept
    /// of its damage, so a 1-point chip cannot stagger. Everything else keeps
    /// its authored poise.
    pub fn poise(self, guarded: bool, damage: u16, poise: u16) -> u16 {
        if self != Self::Chip || damage == 0 || !guarded {
            return poise;
        }
        let applied = self.damage(guarded, damage);
        (u32::from(poise) * u32::from(applied) / u32::from(damage)) as u16
    }
}

/// How the defender's pools take the health damage of a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitRoute {
    /// Only the active pool takes the hit, scaled by `config` for a
    /// matching or opposite channel, and a pool that empties forces a swap.
    /// With `carry`, what the emptied pool could not take carries into the
    /// newly active one, so a hit bigger than a pool can still be lethal.
    Stance {
        /// Alignment scaling, regen delays and the forced-swap cooldown.
        config: CombatStanceConfig,
        /// Carry the excess of an emptied pool into the other one.
        carry: bool,
    },
    /// The strike's channel takes the hit after `guard` scaling, and only
    /// the excess spills into the other pool.
    Spill {
        /// How the defender's active channel scales the strike.
        guard: Guard,
    },
}

/// The defender's posture rules for one hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoiseRule {
    /// Posture damage that breaks.
    pub capacity: u16,
    /// Armour doubles the capacity for this hit.
    pub armored: bool,
    /// The break-grace window of [`CombatFlow`] applies.
    pub grace: bool,
    /// The defender cannot be broken right now (already in a break).
    pub blocked: bool,
}

/// Everything [`Combatant::receive_hit`] needs: the strike and the
/// defender's routing and posture rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitIn {
    /// The attacker's half.
    pub strike: Strike,
    /// How the pools take the damage.
    pub route: HitRoute,
    /// How posture takes the poise.
    pub poise: PoiseRule,
}

/// What one [`Combatant::receive_hit`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HitOut {
    /// Health actually removed from both pools together.
    pub damage_applied: u16,
    /// Both pools are empty after the hit.
    pub defeated: bool,
    /// This hit emptied the last pool. Posture is left alone when it does.
    pub killed: bool,
    /// The hit broke posture.
    pub poise_broken: bool,
}

/// Health, posture, energy and stance of one fighter.
///
/// `Copy` and `const`-constructible so entity tables can hold an array of
/// them in zero-initialised scene storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combatant {
    /// The two vitality pools.
    pub vitality: DualVitality,
    /// Accumulated posture damage.
    pub poise: Poise,
    /// Energy, poise-break grace and the ranged/melee resource cycle.
    pub flow: CombatFlow,
    /// Active side, swap cooldown and swap clock.
    pub stance: CombatStance,
}

impl Combatant {
    /// Empty pools on side One with no timers running. Not a live fighter
    /// until [`Self::reset`] or [`Self::respawn`] gives it health.
    pub const EMPTY: Self = Self {
        vitality: DualVitality::from_pools(VitalityPool::at(0, 0), VitalityPool::at(0, 0)),
        poise: Poise::EMPTY,
        flow: CombatFlow::FULL,
        stance: CombatStance::new(VitalityChannelId::One),
    };

    /// A fresh life: both pools full at `maximum`, posture clear, energy
    /// full, side One with no cooldown and no swap showing.
    pub fn reset(&mut self, maximum: [u16; 2]) {
        self.respawn(maximum);
        self.stance = CombatStance::new(VitalityChannelId::One);
    }

    /// Like [`Self::reset`] but keeps the stance side and its cooldown, only
    /// ending any swap presentation. This is what an entity slot does when
    /// it spawns. A zero maximum gives an empty pool, as the cook allows.
    pub fn respawn(&mut self, maximum: [u16; 2]) {
        self.vitality = DualVitality::from_pools(
            VitalityPool::at(maximum[0], maximum[0]),
            VitalityPool::at(maximum[1], maximum[1]),
        );
        self.poise = Poise::EMPTY;
        self.flow = CombatFlow::FULL;
        self.stance.settle();
    }

    /// Current health of both pools.
    pub fn health(&self) -> [u16; 2] {
        [
            self.vitality.pool(VitalityChannelId::One).current(),
            self.vitality.pool(VitalityChannelId::Two).current(),
        ]
    }

    /// Advance posture recovery and the energy timers by `delta` ticks.
    /// Break grace only drains once `recovering` (a hit reaction) is over.
    pub fn tick(&mut self, delta: u16, recovering: bool) {
        self.poise.tick(delta);
        self.flow.tick(delta, recovering);
    }

    /// Advance the swap cooldown and swap clock by `delta` ticks, without
    /// inactive-pool recovery.
    pub fn advance_stance(&mut self, delta: u16) {
        self.stance.cool_down(delta);
        self.stance.advance_swap(delta);
    }

    /// One fixed tick of the stance with inactive-pool recovery: cooldown,
    /// swap clock and regeneration under `config`.
    pub fn tick_stance(&mut self, config: &CombatStanceConfig, regen_bonus_q12: u16) {
        self.stance
            .tick(&mut self.vitality, config, regen_bonus_q12);
    }

    /// Voluntary swap arming `cooldown_ticks` of cooldown. False when the
    /// cooldown is running or the other side is broken.
    pub fn request_swap(&mut self, cooldown_ticks: u16) -> bool {
        self.stance.try_swap(cooldown_ticks)
    }

    /// Whether the latest swap began no more than `frames` ticks ago.
    pub fn in_swap_window(&self, frames: u16) -> bool {
        self.stance.swap_elapsed_ticks() <= frames
    }

    /// Take one hit: health through `hit.route`, then posture through
    /// `hit.poise`. This is the only way damage reaches a fighter.
    ///
    /// A hit that kills leaves posture alone. A zero-damage hit (poise only)
    /// never touches the pools.
    pub fn receive_hit(&mut self, hit: HitIn) -> HitOut {
        let strike = hit.strike;
        let [one, two] = self.health();
        let before = u32::from(one) + u32::from(two);
        let was_defeated = self.vitality.is_defeated();
        let mut poise = strike.poise;
        if strike.damage != 0 {
            match hit.route {
                HitRoute::Stance { config, carry } => {
                    self.take_stance_hit(strike.channel, strike.damage, &config, carry);
                }
                HitRoute::Spill { guard } => {
                    let guarded = strike.channel == self.stance.active();
                    poise = guard.poise(guarded, strike.damage, poise);
                    let damage = guard.damage(guarded, strike.damage);
                    self.vitality.apply_spill(strike.channel, damage);
                }
            }
        }
        let [one, two] = self.health();
        let after = u32::from(one) + u32::from(two);
        let defeated = self.vitality.is_defeated();
        let killed = !was_defeated && defeated;
        let poise_broken = !killed
            && self.poise_hit(
                poise,
                hit.poise.capacity,
                hit.poise.armored,
                hit.poise.grace,
                hit.poise.blocked,
            );
        HitOut {
            damage_applied: before.saturating_sub(after).min(u32::from(u16::MAX)) as u16,
            defeated,
            killed,
            poise_broken,
        }
    }

    /// Stance routing for [`Self::receive_hit`], with the optional carry of
    /// an emptied pool's excess into the newly active one.
    fn take_stance_hit(
        &mut self,
        channel: VitalityChannelId,
        damage: u16,
        config: &CombatStanceConfig,
        carry: bool,
    ) {
        let active = self.stance.active();
        let room = u32::from(self.vitality.pool(active).current());
        let first = self.take_stance_damage(channel, damage, config);
        if !carry || first.defeated || self.vitality.pool(active).current() != 0 {
            return;
        }
        // The pool ran dry. Convert what it could not take back into hit
        // units (the stance scales an aligned hit before the pool sees it).
        let scale = u32::from(config.aligned_damage_q12).max(1);
        let one = u32::from(VITALITY_Q12_ONE);
        let scaled = u32::from(damage) * scale / one;
        let Some(rest) = scaled.checked_sub(room).filter(|rest| *rest > 0) else {
            return;
        };
        let rest = (rest * one).div_ceil(scale).min(u32::from(u16::MAX)) as u16;
        let other = self.stance.active();
        self.take_stance_damage(other, rest, config);
    }

    /// Damage routed through the stance: only the active pool takes it,
    /// scaled by `config` for a matching or opposite `attack`, and a pool
    /// that empties forces a swap.
    fn take_stance_damage(
        &mut self,
        attack: VitalityChannelId,
        damage: u16,
        config: &CombatStanceConfig,
    ) -> StanceDamageOutcome {
        self.stance
            .apply_damage(&mut self.vitality, attack, damage, config)
    }

    /// Apply `amount` posture damage against `capacity`; true on a break.
    ///
    /// `blocked` refuses the hit outright (the fighter is already in a
    /// break reaction). `grace` makes the break-grace window of
    /// [`CombatFlow`] apply: a hit inside it cannot break, and a break opens
    /// a new one.
    fn poise_hit(
        &mut self,
        amount: u16,
        capacity: u16,
        armored: bool,
        grace: bool,
        blocked: bool,
    ) -> bool {
        if blocked || (grace && !self.flow.can_interrupt()) {
            return false;
        }
        let broken = self.poise.hit(amount, capacity, armored);
        if broken && grace {
            self.flow.broke();
        }
        broken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat_flow::BREAK_GRACE_TICKS;

    fn fighter() -> Combatant {
        let mut c = Combatant::EMPTY;
        c.reset([100, 100]);
        c
    }

    #[test]
    fn reset_is_a_fresh_life_and_respawn_keeps_the_side() {
        let mut c = fighter();
        assert_eq!(c.health(), [100, 100]);
        assert!(c.request_swap(30));
        c.vitality.apply_spill(VitalityChannelId::One, 150);
        c.flow.spend_shot();
        c.respawn([80, 0]);
        assert_eq!(c.health(), [80, 0]);
        assert_eq!(c.flow, CombatFlow::FULL);
        assert_eq!(c.stance.active(), VitalityChannelId::Two);
        assert_eq!(c.stance.swap_cooldown(), 30);
        assert_eq!(c.stance.swap_elapsed_ticks(), u16::MAX);
        c.reset([100, 100]);
        assert_eq!(c, fighter());
    }

    #[test]
    fn swap_window_counts_from_the_swap() {
        let mut c = fighter();
        assert!(!c.in_swap_window(8));
        assert!(c.request_swap(30));
        assert!(!c.request_swap(30), "cooldown refuses a second swap");
        c.advance_stance(8);
        assert!(c.in_swap_window(8));
        c.advance_stance(1);
        assert!(!c.in_swap_window(8));
        assert_eq!(c.stance.swap_cooldown(), 21);
    }

    #[test]
    fn poise_grace_is_a_parameter_not_a_side() {
        let mut graced = fighter();
        assert!(graced.poise_hit(60, 60, false, true, false));
        assert!(
            !graced.poise_hit(60, 60, false, true, false),
            "inside grace"
        );
        graced.tick(BREAK_GRACE_TICKS, false);
        assert!(graced.poise_hit(60, 60, false, true, false));

        let mut legacy = fighter();
        assert!(legacy.poise_hit(60, 60, false, false, false));
        assert!(legacy.poise_hit(60, 60, false, false, false), "no grace");
        assert!(!legacy.poise_hit(60, 60, false, false, true), "blocked");
    }

    fn strike(channel: VitalityChannelId, damage: u16, poise: u16) -> Strike {
        Strike {
            attacker: CombatantId::entity(3),
            kind: HitKind::Melee,
            channel,
            damage,
            poise,
        }
    }

    const POISE_60: PoiseRule = PoiseRule {
        capacity: 60,
        armored: false,
        grace: false,
        blocked: false,
    };

    fn spill(guard: Guard, strike: Strike) -> HitIn {
        HitIn {
            strike,
            route: HitRoute::Spill { guard },
            poise: POISE_60,
        }
    }

    #[test]
    fn spill_route_scales_by_the_guard_then_spills() {
        let mut c = fighter();
        // Guarded (side One is active): a 1 to 3 point chip, poise scaled
        // by the same fraction.
        let out = c.receive_hit(spill(Guard::Chip, strike(VitalityChannelId::One, 32, 64)));
        assert_eq!(out.damage_applied, 2);
        assert!(!out.poise_broken, "64 * 2 / 32 = 4 poise");
        // Exposed: 1.5x, spilling the excess into the other pool.
        let out = c.receive_hit(spill(Guard::Chip, strike(VitalityChannelId::Two, 80, 0)));
        assert_eq!(out.damage_applied, 120);
        assert_eq!(c.health(), [78, 0]);
        let out = c.receive_hit(spill(Guard::Raw, strike(VitalityChannelId::One, 78, 60)));
        assert!(out.killed && out.defeated);
        assert!(!out.poise_broken, "a killing blow leaves posture alone");
    }

    #[test]
    fn stance_route_carries_only_when_asked() {
        let config = CombatStanceConfig::DEFAULT;
        let hazard = |carry| HitIn {
            strike: strike(VitalityChannelId::One, 1000, 0),
            route: HitRoute::Stance { config, carry },
            poise: POISE_60,
        };
        let mut plain = fighter();
        let out = plain.receive_hit(hazard(false));
        assert_eq!(plain.health(), [0, 100]);
        assert!(!out.defeated, "the forced swap absorbs the rest");
        let mut carried = fighter();
        let out = carried.receive_hit(hazard(true));
        assert_eq!(carried.health(), [0, 0]);
        assert!(out.defeated && out.killed);
    }

    #[test]
    fn a_poise_only_hit_leaves_the_pools_alone() {
        let mut c = fighter();
        let hit = HitIn {
            strike: strike(VitalityChannelId::Two, 0, 60),
            route: HitRoute::Stance {
                config: CombatStanceConfig::DEFAULT,
                carry: false,
            },
            poise: POISE_60,
        };
        let out = c.receive_hit(hit);
        assert_eq!(c.health(), [100, 100]);
        assert_eq!(out.damage_applied, 0);
        assert!(out.poise_broken);
    }
}
