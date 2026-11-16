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

use crate::combat_flow::CombatFlow;
use crate::poise::Poise;
use crate::vitality::{
    CombatStance, CombatStanceConfig, DualVitality, StanceDamageOutcome, VitalityChannelId,
    VitalityPool,
};

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

    /// Damage routed through the stance: only the active pool takes it,
    /// scaled by `config` for a matching or opposite `attack`, and a pool
    /// that empties forces a swap.
    pub fn take_stance_damage(
        &mut self,
        attack: VitalityChannelId,
        damage: u16,
        config: &CombatStanceConfig,
    ) -> StanceDamageOutcome {
        self.stance
            .apply_damage(&mut self.vitality, attack, damage, config)
    }

    /// Damage to `channel`, spilling only the excess into the other pool.
    /// True when this hit emptied the last pool.
    pub fn take_spill_damage(&mut self, channel: VitalityChannelId, damage: u16) -> bool {
        self.vitality.apply_spill(channel, damage).actor_defeated
    }

    /// Apply `amount` posture damage against `capacity`; true on a break.
    ///
    /// `blocked` refuses the hit outright (the fighter is already in a
    /// break reaction). `grace` makes the break-grace window of
    /// [`CombatFlow`] apply: a hit inside it cannot break, and a break opens
    /// a new one.
    pub fn poise_hit(
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
        c.take_spill_damage(VitalityChannelId::One, 150);
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
}
