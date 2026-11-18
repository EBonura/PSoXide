//! Optional Cortex prototype; legacy records retain their original rules.
use super::*;
use crate::combat_intent::{CombatAction, CombatIntent, CombatMove};
impl<const N: usize> GameEntities<N> {
    /// Ranged-exchange role of entity `index` (cover, peek, evade ...), 0 when unknown.
    pub fn combat_role(&self, index: usize) -> u8 {
        self.tactics.get(index).map_or(0, |t| t.combat_role)
    }
    /// Record the nearest incoming projectile threat for each entity.
    pub fn observe_projectiles<const P: usize>(
        &mut self,
        records: &[LevelGameEntityRecord],
        projectiles: &crate::projectiles::CombatProjectiles<P>,
    ) {
        for (i, r) in records.iter().enumerate().take(self.count()) {
            if self.away(i) {
                self.projectile_threats[i] = None;
                continue;
            }
            self.projectile_threats[i] = projectiles.incoming_threat(
                self.teams[i],
                r.room,
                self.position(i),
                i32::from(self.body(r).radius),
                i32::from(self.body(r).height),
            );
        }
    }
    /// Record the nearest bolt coming at the actor's body, for her tactical
    /// controller ([`Self::actor_intent`]): the reading every entity gets
    /// from [`Self::observe_projectiles`], on the player's side.
    pub fn observe_actor_projectiles<const P: usize>(
        &mut self,
        persona: &LevelGameEntityRecord,
        projectiles: &crate::projectiles::CombatProjectiles<P>,
    ) {
        let i = Self::ACTOR_SLOT;
        self.projectile_threats[i] = projectiles.incoming_threat(
            crate::projectiles::CombatTeam::Player,
            persona.room,
            self.position(i),
            i32::from(self.body(persona).radius),
            i32::from(self.body(persona).height),
        );
    }
    /// The ranged exchange as a controller: cover, peek and bullet evasion.
    /// None when the exchange wants to fire (the caller's attack decision
    /// takes over); otherwise the plan for this tick.
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn plan_ranged_exchange(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        delta: u16,
        far: i32,
        stats: &mut GameEntityTickStats,
    ) -> Option<tactics::TacticalPlan> {
        let mut intent = CombatIntent::IDLE;
        let before = self.position(i);
        let evade = self.evade_move(r);
        self.exchanges[i].set_evade(evade);
        let energy = self.combatants[i].flow.energy;
        let order = self.exchanges[i].decide(
            before,
            self.tactics[i].last_seen,
            i32::from(self.body(r).radius),
            i32::from(self.body(r).height),
            far,
            energy,
            self.projectile_threats[i],
            &mut |a, b| mover.line_of_sight(r.room, a, b),
        );
        self.tactics[i].combat_role = self.exchanges[i].mode;
        // A dry exchange (nobody can see anybody from cover): close in, the
        // same answer the duel bot gives.
        if order.contest {
            self.combatants[i].flow.contest_space();
        }
        if order.fire {
            return None;
        }
        let goal = if order.evade {
            EnemyGoal::Evade
        } else if self.exchanges[i].mode == crate::ranged_tactics::PEEK {
            EnemyGoal::Peek
        } else {
            EnemyGoal::TakeCover
        };
        let starting = self.tactics[i].goal != goal;
        if starting {
            self.begin_goal(i, goal, 240, 12);
        }
        self.release_attack_owner(i, 0);
        if !order.moving {
            self.plan_face(i, &mut intent, input.target.position, delta);
            self.set_intent(i, GameEntityIntent::Hold);
            stats.holding = stats.holding.saturating_add(1);
            return Some(tactics::TacticalPlan::run(intent));
        }
        let facing_target = if order.evade {
            input.target.position
        } else {
            order.destination
        };
        let facing_error = self.plan_face(i, &mut intent, facing_target, delta);
        if !order.evade && facing_error > 683 {
            return Some(tactics::TacticalPlan::run(intent));
        }
        let facing = self
            .tactical_turn(i, facing_target, delta)
            .map_or(self.yaw_raw(i), |(yaw, _)| yaw);
        self.tactics[i].running =
            !order.evade && Self::can_run(r) && Self::distance(before, order.destination) > 96;
        // Cover lanes have already been checked. Follow that segment directly;
        // the chase motor can retain its previous eight-way heading past a peek.
        let dx = order.destination[0] - before[0];
        let dz = order.destination[2] - before[2];
        let running = self.tactics[i].running;
        intent.movement = self.move_toward(r, dx, dz, delta, running);
        if running {
            intent.held = intent.held.with(CombatAction::Evade);
        }
        // Dodging a bolt is the evade every fighter has: a roll (with its
        // i-frames) for one with the mobility kit, pressed as the dodge
        // starts; without the kit it is a walk aside.
        if order.evade && starting {
            intent.pressed = intent.pressed.with(CombatAction::Evade);
        }
        self.set_intent(
            i,
            if order.evade {
                let yaw = psx_engine::Angle::from_q12(facing as u16);
                if yaw.cos().mul_i32(dx) - yaw.sin().mul_i32(dz) < 0 {
                    GameEntityIntent::CircleLeft
                } else {
                    GameEntityIntent::CircleRight
                }
            } else {
                GameEntityIntent::Approach
            },
        );
        Some(tactics::TacticalPlan::run(intent))
    }
    /// Melee defence as a controller: back off from a telegraphed swing.
    /// None when there is nothing to defend against this tick.
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn plan_melee_defense(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        delta: u16,
        stats: &mut GameEntityTickStats,
    ) -> Option<tactics::TacticalPlan> {
        let mut intent = CombatIntent::IDLE;
        self.tactics[i].defend_cooldown = self.tactics[i].defend_cooldown.saturating_sub(delta);
        self.tactics[i].defend_ticks = self.tactics[i].defend_ticks.saturating_sub(delta);
        if !self.target_in_line_of_sight(r, i, input, mover) {
            return None;
        }
        let reach = self.melee_attack_reach(r, input);
        if input.target.attack_read == 3 {
            if self.tactics[i].defend_ticks > 0 && self.tactics[i].goal == EnemyGoal::Retreat {
                self.finish_goal(i, EnemyGoalResult::Cancelled);
            }
            self.tactics[i].defend_ticks = 0;
            self.tactics[i].combat_role = 7;
            return None;
        }
        if input.target.attack_read == 1
            && self.tactics[i].defend_cooldown == 0
            && self.target_within(i, input, reach * 2)
        {
            self.tactics[i].defend_ticks = 24;
            self.tactics[i].defend_cooldown = 90;
        }
        if self.tactics[i].defend_ticks == 0 {
            return None;
        }
        self.tactics[i].combat_role = 6;
        self.release_attack_owner(i, 0);
        if self.tactics[i].goal != EnemyGoal::Retreat {
            self.begin_goal(i, EnemyGoal::Retreat, 24, reach + 32);
        }
        self.plan_face(i, &mut intent, input.target.position, delta);
        if self.target_within(i, input, reach + 32) {
            let before = self.position(i);
            let yaw = psx_engine::Angle::from_q12(atan2_q12(
                before[0] - input.target.position[0],
                before[2] - input.target.position[2],
            ));
            intent.movement = CombatMove::Move {
                x: yaw.sin().raw() as i16,
                z: yaw.cos().raw() as i16,
                forward: 0,
            };
        }
        self.set_intent(i, GameEntityIntent::Retreat);
        stats.retreating = stats.retreating.saturating_add(1);
        Some(tactics::TacticalPlan::run(intent))
    }
    /// Current energy of entity `index`, 0 when out of range.
    pub fn energy(&self, index: usize) -> u16 {
        self.combatants.get(index).map_or(0, |c| c.flow.energy)
    }
    /// True when entity `index` can afford a shot under its `record`'s stats.
    pub fn can_fire_energy(&self, record: &LevelGameEntityRecord, index: usize) -> bool {
        self.combatants
            .get(index)
            .is_some_and(|c| c.flow.can_shoot(&self.body(record).combat))
    }
    /// Report that a shot by entity `index` landed; `interrupted` marks an interrupting hit.
    pub fn projectile_connected(&mut self, index: usize, interrupted: bool) {
        if let Some(f) = self.combatants.get_mut(index).map(|c| &mut c.flow) {
            f.shot_hit(interrupted);
        }
    }
    /// Charge one shot to entity `index` at its `record`'s shot cost.
    pub fn spend_shot_energy(&mut self, record: &LevelGameEntityRecord, index: usize) {
        let stats = self.body(record).combat;
        if let Some(c) = self.combatants.get_mut(index) {
            c.flow.spend_shot(&stats);
        }
    }
    /// Credit fighter `index` (an entity or the actor slot) for a connected
    /// melee hit at its own gain ([`Self::stats`]).
    pub fn gain_melee_energy(
        &mut self,
        records: &[LevelGameEntityRecord],
        index: usize,
        heavy: bool,
    ) {
        if !self.holds_fighter(index) {
            return;
        }
        let stats = *self.stats(records, index);
        self.combatants[index].flow.melee_hit(heavy, &stats);
    }
    /// True when entity `index` is in the second half of a windup and may be broken by a bolt.
    pub fn shot_opening(&self, records: &[LevelGameEntityRecord], index: usize) -> bool {
        index < self.count()
            && index < records.len()
            && self.attack_phase(index) == Some(AttackPhase::Windup)
            && self.state_ticks[index] >= self.selected_timing(index).windup / 2
            && self.combatants[index].flow.can_interrupt()
    }
    /// Apply a projectile hit; the bolt's poise comes from `CombatFlow::shot_poise`.
    /// `crit` marks a bolt of a crit volley ([`Strike::crit`]).
    pub fn apply_projectile_hit(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        channel: VitalityChannelId,
        damage: u16,
        poise: u16,
        crit: bool,
    ) -> GameEntityHitOutcome {
        self.apply_projectile_hit_by(
            records,
            index,
            CombatantId::PLAYER,
            channel,
            damage,
            poise,
            crit,
        )
    }

    /// [`Self::apply_projectile_hit`] for a bolt `by` any fighter: an
    /// entity's bolt on its rival, whose parry staggers that shooter.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_projectile_hit_by(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        by: CombatantId,
        channel: VitalityChannelId,
        damage: u16,
        poise: u16,
        crit: bool,
    ) -> GameEntityHitOutcome {
        if index >= self.count() || index >= records.len() {
            return GameEntityHitOutcome::MISS;
        }
        let strike = self.projectile_strike(records, index, by, channel, damage, poise, crit);
        self.receive_hit(records, index, strike)
    }

    /// The strike a bolt `by` any fighter lands on entity `index`: its
    /// posture damage through the shot rule (`CombatFlow::shot_poise`, an
    /// open windup takes more), the rest as the bolt carries it.
    #[allow(clippy::too_many_arguments)]
    pub fn projectile_strike(
        &self,
        records: &[LevelGameEntityRecord],
        index: usize,
        by: CombatantId,
        channel: VitalityChannelId,
        damage: u16,
        poise: u16,
        crit: bool,
    ) -> Strike {
        let poise = match records.get(index).filter(|_| index < self.count()) {
            Some(record) => self.combatants[index].flow.shot_poise(
                poise,
                self.body(record).combat.poise,
                self.shot_opening(records, index),
            ),
            None => poise,
        };
        Strike {
            attacker: by,
            ..super::player_strike(HitKind::Projectile, channel, damage, poise, crit)
        }
    }
}
