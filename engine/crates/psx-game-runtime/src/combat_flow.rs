//! Prototype resource and interruption rules shared by player and NPCs.
/// All rates use the 60 Hz simulation clock, independent of rendering.
pub const ENERGY_MAX: u16 = 100;
pub const SHOT_COST: u16 = 20;
/// AI rebuilds a useful three-shot reserve before leaving recovery melee.
pub const AI_RESUME_ENERGY: u16 = 60;
/// Preserve a contact opportunity through recovery and the stance cooldown.
pub const FOLLOWUP_TICKS: u16 = 480;
/// Hold a melee response long enough to complete an attack after a failed escape.
pub const CONTEST_SPACE_TICKS: u16 = 240;
pub const MELEE_GAIN: u16 = 12;
pub const HEAVY_GAIN: u16 = 20;
pub const FLOAT_GAIN_PER_SECOND: u16 = 20;
pub const AIR_TICKS: u16 = 360;
pub const GROUND_REARM_TICKS: u16 = 120;
pub const BREAK_GRACE_TICKS: u16 = 60;

/// Ground sidestep tuning, on the 60 Hz simulation clock.
pub const EVADE_DISTANCE: i32 = 72;
pub const EVADE_MOVE_TICKS: u16 = 18;
pub const EVADE_RECOVERY_TICKS: u16 = 12;
pub const EVADE_COOLDOWN_TICKS: u16 = 120;

/// Actual firing space: several body lengths beyond contact, bounded by the weapon.
pub fn ranged_band(preferred: i32, melee_reach: i32, weapon_max: i32) -> (i32, i32) {
    let far = preferred
        .saturating_mul(6)
        .max(melee_reach.saturating_mul(4))
        .min(weapon_max.max(1));
    let near = preferred
        .saturating_mul(3)
        .max(melee_reach.saturating_mul(3))
        .min(far.saturating_mul(3) / 4)
        .max(1);
    (near, far.max(near))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatFlow {
    pub energy: u16,
    pub break_grace: u16,
    pub air_left: u16,
    pub followup: u8,
    followup_ticks: u16,
    recharging: bool,
    ranged_phase: bool,
    contest_ticks: u16,
    ground_ticks: u16,
    float_fraction: u16,
}
impl CombatFlow {
    pub const FULL: Self = Self {
        energy: ENERGY_MAX,
        break_grace: 0,
        air_left: AIR_TICKS,
        followup: 0,
        followup_ticks: 0,
        recharging: false,
        ranged_phase: true,
        contest_ticks: 0,
        ground_ticks: 0,
        float_fraction: 0,
    };
    pub fn tick(&mut self, delta: u16, recovering: bool) {
        self.contest_ticks = self.contest_ticks.saturating_sub(delta);
        self.followup_ticks = self.followup_ticks.saturating_sub(delta);
        if self.followup_ticks == 0 {
            self.followup = 0;
        }
        // Grace begins after the authored reaction finishes.
        if !recovering {
            self.break_grace = self.break_grace.saturating_sub(delta);
        }
    }
    pub fn can_shoot(&self) -> bool {
        self.energy >= SHOT_COST
    }
    /// Charge only successfully allocated projectiles, including each burst emitter.
    pub fn spend_shot(&mut self) -> bool {
        if !self.can_shoot() {
            return false;
        }
        self.energy -= SHOT_COST;
        self.recharging |= self.energy < SHOT_COST;
        if self.recharging {
            self.ranged_phase = false;
        }
        if self.followup == 1 {
            self.followup = 0;
        }
        true
    }
    /// Contacts, never swings at air or defeated bodies, earn energy.
    pub fn melee_hit(&mut self, heavy: bool) {
        if heavy {
            self.followup = 1;
            self.followup_ticks = FOLLOWUP_TICKS;
        } else if self.followup == 2 {
            self.followup = 0;
        }
        self.energy = self
            .energy
            .saturating_add(if heavy { HEAVY_GAIN } else { MELEE_GAIN })
            .min(ENERGY_MAX);
        if self.energy >= AI_RESUME_ENERGY {
            self.recharging = false;
            self.ranged_phase = true;
        }
    }
    /// The allowance belongs to the whole airborne excursion. Neither another
    /// arch nor briefly touching ground refreshes it. Shooting pauses recharge.
    pub fn air_tick(&mut self, attached: bool, grounded: bool, firing: bool) -> bool {
        if grounded && !attached {
            self.ground_ticks = self.ground_ticks.saturating_add(1).min(GROUND_REARM_TICKS);
            if self.ground_ticks == GROUND_REARM_TICKS {
                self.air_left = AIR_TICKS;
            }
        } else {
            self.ground_ticks = 0;
        }
        if attached {
            self.air_left = self.air_left.saturating_sub(1);
            if !firing && self.air_left > 0 {
                self.float_fraction += FLOAT_GAIN_PER_SECOND;
                if self.float_fraction >= 60 {
                    self.float_fraction -= 60;
                    self.energy = (self.energy + 1).min(ENERGY_MAX);
                    if self.energy >= AI_RESUME_ENERGY {
                        self.recharging = false;
                        self.ranged_phase = true;
                    }
                }
            }
        }
        attached && self.air_left == 0
    }
    pub fn shot_hit(&mut self, interrupted: bool) {
        if interrupted {
            self.ranged_phase = false;
            self.followup = 2;
            self.followup_ticks = FOLLOWUP_TICKS;
        }
    }
    /// A failed physical escape must produce a response, not helpless wall-running.
    pub fn contest_space(&mut self) {
        self.contest_ticks = CONTEST_SPACE_TICKS;
    }
    /// AI preference is an opportunity, not an automatic attack or forced player switch.
    pub fn preferred_stance(&self, _fallback: u8, separated: bool) -> u8 {
        // Close combat stays melee unless a connected heavy attack creates an
        // exit opportunity; resources still govern whether ranged is affordable.
        if self.needs_melee_energy()
            || self.contest_ticks > 0
            || self.followup == 2
            || !self.ranged_phase
            || (!separated && self.followup != 1)
        {
            0
        } else {
            1
        }
    }
    /// Hysteresis prevents one hit / one shot oscillation after emptying the bar.
    pub fn needs_melee_energy(&self) -> bool {
        self.recharging || !self.can_shoot()
    }
    /// Stable reason codes for the resource-driven cycle, independent of distance.
    pub fn stance_reason(&self, _fallback: u8, _separated: bool) -> u8 {
        if self.needs_melee_energy() {
            5
        } else if self.contest_ticks > 0 {
            10
        } else if self.followup == 2 {
            7
        } else if self.followup == 1 {
            6
        } else if self.ranged_phase {
            9
        } else {
            0
        }
    }
    pub fn can_interrupt(&self) -> bool {
        self.break_grace == 0
    }
    pub fn broke(&mut self) {
        self.break_grace = BREAK_GRACE_TICKS;
    }
    pub fn shot_poise(&self, authored: u16, capacity: u16, exposed: bool) -> u16 {
        if !self.can_interrupt() {
            0
        } else if exposed {
            capacity.max(1)
        } else {
            (authored / 4).min(10)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_escape_commits_to_melee_without_losing_the_ranged_reserve() {
        let mut flow = CombatFlow::FULL;
        flow.contest_space();
        assert_eq!(flow.preferred_stance(1, false), 0);
        flow.melee_hit(true);
        assert_eq!(flow.preferred_stance(1, false), 0);
        flow.tick(CONTEST_SPACE_TICKS, false);
        assert_eq!(flow.energy, ENERGY_MAX);
        assert_eq!(flow.preferred_stance(0, false), 1);
    }
    #[test]
    fn firing_requires_real_separation_and_resource_phase_survives_contact() {
        assert_eq!(ranged_band(64, 50, 1000), (192, 384));
        assert_eq!(ranged_band(64, 50, 128), (96, 128));
        let mut f = CombatFlow::FULL;
        assert_eq!(
            f.preferred_stance(0, false),
            0,
            "close combat stays melee without an opening"
        );
        assert_eq!(f.preferred_stance(1, true), 1);
        f.melee_hit(false);
        assert_eq!(
            f.preferred_stance(1, false),
            0,
            "a light hit does not force an exit from melee"
        );
        for _ in 0..5 {
            f.spend_shot();
        }
        assert_eq!(f.preferred_stance(1, true), 0);
        for _ in 0..3 {
            f.melee_hit(true);
        }
        assert_eq!(
            f.preferred_stance(0, false),
            1,
            "must plan escape before separation exists"
        );
        f.shot_hit(true);
        assert_eq!(f.preferred_stance(1, true), 0);
    }
    #[test]
    fn shots_exhaust_and_only_real_melee_replenishes() {
        let mut f = CombatFlow::FULL;
        for _ in 0..5 {
            assert!(f.spend_shot());
        }
        assert!(!f.spend_shot());
        f.melee_hit(false);
        assert!(!f.can_shoot());
        f.melee_hit(false);
        assert!(f.spend_shot());
        assert_eq!(f.energy, 4);
    }
    #[test]
    fn float_rate_fire_and_rehook_share_one_allowance() {
        let mut f = CombatFlow::FULL;
        f.energy = 0;
        for _ in 0..60 {
            assert!(!f.air_tick(true, false, false));
        }
        assert_eq!(f.energy, 20);
        for _ in 0..60 {
            f.air_tick(true, false, true);
        }
        assert_eq!(f.energy, 20);
        f.air_tick(false, false, false);
        for _ in 0..239 {
            assert!(!f.air_tick(true, false, false));
        }
        assert!(f.air_tick(true, false, false));
        for _ in 0..119 {
            f.air_tick(false, true, false);
        }
        assert_eq!(f.air_left, 0);
        f.air_tick(false, true, false);
        assert_eq!(f.air_left, AIR_TICKS);
    }
    #[test]
    fn ai_rebuilds_a_reserve_without_blocking_human_shots() {
        let mut f = CombatFlow::FULL;
        for _ in 0..5 {
            f.spend_shot();
        }
        f.melee_hit(true);
        assert!(f.can_shoot());
        assert_eq!(f.preferred_stance(1, true), 0);
        f.melee_hit(true);
        assert_eq!(f.preferred_stance(1, true), 0);
        f.melee_hit(true);
        assert_eq!(f.preferred_stance(0, true), 1);
        assert_eq!(f.stance_reason(0, true), 6);
        f.tick(300, false);
        assert_eq!(f.preferred_stance(0, true), 1);
        f.tick(180, false);
        assert_eq!(f.preferred_stance(0, true), 1);
    }
    #[test]
    fn reaction_and_grace_prevent_stun_refresh() {
        let mut f = CombatFlow::FULL;
        assert_eq!(f.shot_poise(40, 60, false), 10);
        assert_eq!(f.shot_poise(40, 60, true), 60);
        f.broke();
        f.tick(200, true);
        assert_eq!(f.shot_poise(40, 60, true), 0);
        f.tick(59, false);
        assert!(!f.can_interrupt());
        f.tick(1, false);
        assert!(f.can_interrupt());
    }
}
