//! Optional Cortex prototype; legacy records retain their original rules.
use super::*;
use crate::combat_intent::{CombatAction, CombatIntent, CombatMove};
impl<const N: usize, const S: bool, const T: bool> GameEntities<N, S, T> {
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
            self.projectile_threats[i] = projectiles.incoming_threat(
                self.teams[i],
                r.room,
                self.position(i),
                i32::from(r.radius),
                i32::from(r.height),
            );
        }
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
            i32::from(r.radius),
            i32::from(r.height),
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
        let reach = Self::melee_attack_reach(r, input);
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
            .is_some_and(|c| c.flow.can_shoot(&record.combat))
    }
    /// Report that a shot by entity `index` landed; `interrupted` marks an interrupting hit.
    pub fn projectile_connected(&mut self, index: usize, interrupted: bool) {
        if let Some(f) = self.combatants.get_mut(index).map(|c| &mut c.flow) {
            f.shot_hit(interrupted);
        }
    }
    /// Charge one shot to entity `index` at its `record`'s shot cost.
    pub fn spend_shot_energy(&mut self, record: &LevelGameEntityRecord, index: usize) {
        if let Some(c) = self.combatants.get_mut(index) {
            c.flow.spend_shot(&record.combat);
        }
    }
    /// Credit entity `index` for a connected melee hit at its `record`'s gain.
    pub fn gain_melee_energy(&mut self, record: &LevelGameEntityRecord, index: usize, heavy: bool) {
        if let Some(c) = self.combatants.get_mut(index) {
            c.flow.melee_hit(heavy, &record.combat);
        }
    }
    /// True when entity `index` is in the second half of a windup and may be broken by a bolt.
    pub fn shot_opening(&self, records: &[LevelGameEntityRecord], index: usize) -> bool {
        index < self.count()
            && index < records.len()
            && self.state(index) == GameEntityState::Windup
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
        let poise = self.combatants[index].flow.shot_poise(
            poise,
            records[index].combat.poise,
            self.shot_opening(records, index),
        );
        let strike = Strike {
            attacker: by,
            ..super::player_strike(HitKind::Projectile, channel, damage, poise, crit)
        };
        self.receive_hit(records, index, strike)
    }
    /// A heavy connection creates separation through the same body collision as AI walking.
    pub fn recoil_from(&mut self, index: usize, from: [i32; 3]) {
        if index >= self.count() || self.state(index) == GameEntityState::Dead {
            return;
        }
        let yaw = psx_engine::Angle::from_q12(atan2_q12(
            self.px(index) - from[0],
            self.pz(index) - from[2],
        ));
        self.recoil[index] = [yaw.sin().mul_i32(5) as i16, yaw.cos().mul_i32(5) as i16, 8];
    }
    pub(super) fn step_recoil(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        mover: &mut impl FighterWorld,
        delta: u16,
    ) {
        let [dx, dz, ticks] = self.recoil[i];
        let step = delta.min(ticks.max(0) as u16);
        if step == 0 {
            return;
        }
        self.recoil[i][2] -= step as i16;
        let p = mover.push_body(
            i,
            r.room,
            self.position(i),
            i32::from(dx) * i32::from(step),
            i32::from(dz) * i32::from(step),
            i32::from(r.radius),
            i32::from(r.height),
        );
        self.place(i, p);
    }
    /// Correct crowding during the first half of a melee tell, then plant.
    /// Collision and a fixed turn rate apply; attacks never track through impact.
    pub(super) fn step_melee_tell(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        delta: u16,
    ) {
        if self.selected_attack_is_ranged(i) {
            return;
        }
        self.tactics[i].moved = false;
        self.tactics[i].running = false;
        if self.state_ticks[i] >= self.selected_timing(i).windup / 2
            || !self.target_in_line_of_sight(r, i, input, mover)
        {
            return;
        }
        self.face_tactical(i, input.target.position, delta);
        if !self.target_within(i, input, Self::melee_attack_reach(r, input) + 8) {
            return;
        }
        let before = self.position(i);
        let away = psx_engine::Angle::from_q12(atan2_q12(
            before[0] - input.target.position[0],
            before[2] - input.target.position[2],
        ));
        // Back off at a walk, still facing the target: the motor's
        // locked-on backward pace.
        let facing = Angle::from_q12(self.yaw_raw(i) as u16);
        self.request_heading(away, 4096, Some(facing), false);
        self.drive(r, i, mover, delta);
        self.tactics[i].goal = tactics::EnemyGoal::Retreat;
        self.advance_tactical_animation(r, i, before, delta);
    }
    /// Locomotion clip and phase to show while a moving entity winds up or fires; `None` otherwise.
    pub fn firing_gait(&self, r: &LevelGameEntityRecord, i: usize) -> Option<(u16, u16)> {
        (i < self.count()
            && (self.selected_attack_is_ranged(i) || self.state(i) == GameEntityState::Windup)
            && matches!(
                self.state(i),
                GameEntityState::Windup | GameEntityState::Attack | GameEntityState::Recover
            )
            && self.tactics[i].moved)
            .then(|| {
                (
                    self.tactical_locomotion_clip(r, i),
                    (self.tactics[i].animation_phase_q8 >> 8) as u16,
                )
            })
    }
    pub(super) fn step_firing(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl FighterWorld,
        delta: u16,
    ) {
        if !Self::is_tactical(r) || !self.selected_attack_is_ranged(i) {
            return;
        }
        let before = self.position(i);
        self.tactics[i].moved = false;
        self.tactics[i].running = false;
        if !self.target_in_line_of_sight(r, i, input, mover) {
            return;
        }
        let dx = input.target.position[0] - before[0];
        let dz = input.target.position[2] - before[2];
        let toward = psx_engine::Angle::from_q12(atan2_q12(dx, dz));
        let (near, _) = crate::combat_flow::ranged_band(
            i32::from(r.preferred_distance),
            Self::melee_attack_reach(r, input) + 16,
            i32::from(r.attack_max_range),
        );
        let retreat = self.target_within(i, input, near);
        let side = if i & 1 == 0 { 1024 } else { 3072 };
        let yaw = toward.add(psx_engine::Angle::from_q12(if retreat {
            2048
        } else {
            side
        }));
        self.tactics[i].goal = if retreat {
            tactics::EnemyGoal::Retreat
        } else if side == 1024 {
            tactics::EnemyGoal::CircleRight
        } else {
            tactics::EnemyGoal::CircleLeft
        };
        // Half the spacing pace while committing a shot, still facing the
        // target; never sprint away. The motor carries the sub-unit steps.
        let strength = 4096 * i32::from(r.spacing_speed_percent.clamp(1, 100)) / 200;
        let facing = Angle::from_q12(self.yaw_raw(i) as u16);
        self.request_heading(yaw, strength, Some(facing), false);
        self.drive(r, i, mover, delta);
        self.advance_tactical_animation(r, i, before, delta);
    }
}
