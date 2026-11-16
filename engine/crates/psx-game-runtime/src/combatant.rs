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
//! [`CombatStats`], [`HitRoute`] and [`PoiseRule`]. Damage and poise follow
//! one rule for every fighter, [`stance_scaled`]: a hit from the stance
//! opposite the defender's active one is multiplied by the defender's
//! opposite-stance multipliers, a matching hit is taken as authored.
//! Characters differ only in their stats. The route still differs (the
//! player's active pool against the enemy's spill) until the two pools
//! become one bar.

use psx_level::CombatStats;

use crate::combat_flow::CombatFlow;
use crate::poise::Poise;
use crate::vitality::{
    CombatStance, CombatStanceConfig, DualVitality, StanceDamageOutcome, VitalityChannelId,
    VitalityPool, VITALITY_Q12_ONE,
};

/// The one stance rule: health and posture damage a hit of `damage` and
/// `poise` deals to a defender with `stats`, `opposite` when the hit's
/// stance is not the defender's active one. Zero stays zero.
pub fn stance_scaled(stats: &CombatStats, opposite: bool, damage: u16, poise: u16) -> (u16, u16) {
    if !opposite {
        return (damage, poise);
    }
    (
        scale_q12(damage, stats.opposite_damage_q12),
        scale_q12(poise, stats.opposite_poise_q12),
    )
}

fn scale_q12(value: u16, q12: u16) -> u16 {
    (u32::from(value) * u32::from(q12) / u32::from(VITALITY_Q12_ONE)).min(u32::from(u16::MAX))
        as u16
}

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

/// How the defender's pools take the health damage of a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
///
/// The damage is already scaled by [`stance_scaled`]; the route only says
/// which pool takes it. This is the last per-side difference in a hit and
/// goes away with the second pool.
pub enum HitRoute {
    /// Only the active pool takes the hit, and a pool that empties forces a
    /// swap. With `carry`, what the emptied pool could not take carries into
    /// the newly active one, so a hit bigger than a pool can still be lethal.
    Stance {
        /// Regen delays and the forced-swap cooldown.
        config: CombatStanceConfig,
        /// Carry the excess of an emptied pool into the other one.
        carry: bool,
    },
    /// The strike's channel takes the hit, and only the excess spills into
    /// the other pool.
    Spill,
}

/// The defender's posture state for one hit. The capacity is
/// [`CombatStats::poise`], and break grace always applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoiseRule {
    /// Armour doubles the capacity for this hit.
    pub armored: bool,
    /// The defender cannot be broken right now (already in a break).
    pub blocked: bool,
}

/// Everything [`Combatant::receive_hit`] needs: the strike and the
/// defender's stats, routing and posture state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitIn {
    /// The attacker's half.
    pub strike: Strike,
    /// The defender's numbers.
    pub stats: CombatStats,
    /// Which pool takes the damage.
    pub route: HitRoute,
    /// The defender's posture state.
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

    /// A fresh life for `stats`: both pools full, posture clear, energy
    /// full, side One with no cooldown and no swap showing.
    pub fn reset(&mut self, stats: &CombatStats) {
        self.respawn(stats);
        self.stance = CombatStance::new(VitalityChannelId::One);
    }

    /// Like [`Self::reset`] but keeps the stance side and its cooldown, only
    /// ending any swap presentation. This is what an entity slot does when
    /// it spawns. A zero maximum gives an empty pool, as the cook allows.
    pub fn respawn(&mut self, stats: &CombatStats) {
        let [one, two] = [stats.max_health, stats.max_health_secondary];
        self.vitality =
            DualVitality::from_pools(VitalityPool::at(one, one), VitalityPool::at(two, two));
        self.poise = Poise::EMPTY;
        self.flow = CombatFlow::full(stats);
        self.stance.settle();
    }

    /// Whether a hit on `channel` comes from the stance opposite this
    /// fighter's active one.
    pub fn opposes(&self, channel: VitalityChannelId) -> bool {
        channel != self.stance.active()
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

    /// Take one hit: health and posture scaled by [`stance_scaled`], health
    /// into the pools through `hit.route`, then posture against
    /// [`CombatStats::poise`]. This is the only way damage reaches a fighter.
    ///
    /// A hit that kills leaves posture alone. A zero-damage hit (poise only)
    /// never touches the pools.
    pub fn receive_hit(&mut self, hit: HitIn) -> HitOut {
        let strike = hit.strike;
        let [one, two] = self.health();
        let before = u32::from(one) + u32::from(two);
        let was_defeated = self.vitality.is_defeated();
        let (damage, poise) = stance_scaled(
            &hit.stats,
            self.opposes(strike.channel),
            strike.damage,
            strike.poise,
        );
        if damage != 0 {
            match hit.route {
                HitRoute::Stance { config, carry } => {
                    self.take_stance_hit(damage, &config, carry);
                }
                HitRoute::Spill => {
                    self.vitality.apply_spill(strike.channel, damage);
                }
            }
        }
        let [one, two] = self.health();
        let after = u32::from(one) + u32::from(two);
        let defeated = self.vitality.is_defeated();
        let killed = !was_defeated && defeated;
        let poise_broken =
            !killed && self.poise_hit(poise, hit.stats.poise, hit.poise.armored, hit.poise.blocked);
        HitOut {
            damage_applied: before.saturating_sub(after).min(u32::from(u16::MAX)) as u16,
            defeated,
            killed,
            poise_broken,
        }
    }

    /// Active-pool routing for [`Self::receive_hit`], with the optional
    /// carry of an emptied pool's excess into the newly active one.
    fn take_stance_hit(&mut self, damage: u16, config: &CombatStanceConfig, carry: bool) {
        let active = self.stance.active();
        let room = self.vitality.pool(active).current();
        let first = self.take_stance_damage(damage, config);
        if !carry || first.defeated || self.vitality.pool(active).current() != 0 {
            return;
        }
        // The pool ran dry; what it could not take lands on the other one.
        let rest = damage.saturating_sub(room);
        if rest > 0 {
            self.take_stance_damage(rest, config);
        }
    }

    /// Damage routed through the stance: only the active pool takes it, and
    /// a pool that empties forces a swap.
    fn take_stance_damage(
        &mut self,
        damage: u16,
        config: &CombatStanceConfig,
    ) -> StanceDamageOutcome {
        self.stance.apply_damage(&mut self.vitality, damage, config)
    }

    /// Apply `amount` posture damage against `capacity`; true on a break.
    ///
    /// `blocked` refuses the hit outright (the fighter is already in a
    /// break reaction). The break-grace window of [`CombatFlow`] applies: a
    /// hit inside it cannot break, and a break opens a new one.
    fn poise_hit(&mut self, amount: u16, capacity: u16, armored: bool, blocked: bool) -> bool {
        if blocked || !self.flow.can_interrupt() {
            return false;
        }
        let broken = self.poise.hit(amount, capacity, armored);
        if broken {
            self.flow.broke();
        }
        broken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat_flow::BREAK_GRACE_TICKS;

    /// 100 per pool, poise 60, the starting multipliers and energy.
    const STATS: CombatStats = CombatStats::PLAYER;

    fn fighter() -> Combatant {
        let mut c = Combatant::EMPTY;
        c.reset(&STATS);
        c
    }

    #[test]
    fn reset_is_a_fresh_life_and_respawn_keeps_the_side() {
        let mut c = fighter();
        assert_eq!(c.health(), [100, 100]);
        assert!(c.request_swap(30));
        c.vitality.apply_spill(VitalityChannelId::One, 150);
        c.flow.spend_shot(&STATS);
        let small = CombatStats {
            max_health: 80,
            max_health_secondary: 0,
            energy_max: 40,
            ..STATS
        };
        c.respawn(&small);
        assert_eq!(c.health(), [80, 0]);
        assert_eq!(c.flow, CombatFlow::full(&small));
        assert_eq!(c.flow.energy, 40, "energy starts at the stats' ceiling");
        assert_eq!(c.stance.active(), VitalityChannelId::Two);
        assert_eq!(c.stance.swap_cooldown(), 30);
        assert_eq!(c.stance.swap_elapsed_ticks(), u16::MAX);
        c.reset(&STATS);
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
    fn poise_grace_applies_to_every_fighter() {
        let mut c = fighter();
        assert!(c.poise_hit(60, 60, false, false));
        assert!(!c.poise_hit(60, 60, false, false), "inside grace");
        c.tick(BREAK_GRACE_TICKS, false);
        assert!(c.poise_hit(60, 60, false, false));
        c.tick(BREAK_GRACE_TICKS, false);
        assert!(!c.poise_hit(60, 60, false, true), "blocked");
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

    const UNARMORED: PoiseRule = PoiseRule {
        armored: false,
        blocked: false,
    };

    fn spill(strike: Strike) -> HitIn {
        HitIn {
            strike,
            stats: STATS,
            route: HitRoute::Spill,
            poise: UNARMORED,
        }
    }

    #[test]
    fn one_stance_rule_scales_only_opposite_hits() {
        assert_eq!(stance_scaled(&STATS, false, 32, 48), (32, 48));
        assert_eq!(stance_scaled(&STATS, true, 32, 48), (40, 60));
        assert_eq!(stance_scaled(&STATS, true, 0, 0), (0, 0));
        let custom = CombatStats {
            opposite_damage_q12: 8192,
            opposite_poise_q12: 2048,
            ..STATS
        };
        assert_eq!(stance_scaled(&custom, true, 32, 48), (64, 24));
    }

    #[test]
    fn spill_route_takes_the_scaled_hit_then_spills() {
        let mut c = fighter();
        // Matching stance (side One is active): taken as authored.
        let out = c.receive_hit(spill(strike(VitalityChannelId::One, 32, 48)));
        assert_eq!(out.damage_applied, 32);
        assert!(!out.poise_broken, "48 of 60");
        assert_eq!(c.health(), [68, 100]);
        // Opposite stance: 1.25x, spilling the excess into the other pool.
        let out = c.receive_hit(spill(strike(VitalityChannelId::Two, 96, 0)));
        assert_eq!(out.damage_applied, 120);
        assert_eq!(c.health(), [48, 0]);
        let out = c.receive_hit(spill(strike(VitalityChannelId::One, 48, 60)));
        assert!(out.killed && out.defeated);
        assert!(!out.poise_broken, "a killing blow leaves posture alone");
    }

    #[test]
    fn an_opposite_hit_breaks_posture_sooner() {
        let mut matching = fighter();
        let out = matching.receive_hit(spill(strike(VitalityChannelId::One, 0, 50)));
        assert!(!out.poise_broken, "50 of 60");
        let mut opposite = fighter();
        let out = opposite.receive_hit(spill(strike(VitalityChannelId::Two, 0, 50)));
        assert!(out.poise_broken, "50 x 1.25 = 62 of 60");
    }

    #[test]
    fn stance_route_carries_only_when_asked() {
        let config = CombatStanceConfig::DEFAULT;
        let hazard = |carry| HitIn {
            strike: strike(VitalityChannelId::One, 1000, 0),
            stats: STATS,
            route: HitRoute::Stance { config, carry },
            poise: UNARMORED,
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
            stats: STATS,
            route: HitRoute::Stance {
                config: CombatStanceConfig::DEFAULT,
                carry: false,
            },
            poise: UNARMORED,
        };
        let out = c.receive_hit(hit);
        assert_eq!(c.health(), [100, 100]);
        assert_eq!(out.damage_applied, 0);
        assert!(out.poise_broken);
    }
}
