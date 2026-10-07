//! Optional Cortex prototype; legacy records retain their original rules.
use super::*;
impl<const N: usize, const S: bool> GameEntities<N, S> {
    /// Visible animation phase supplied by the game: 0 neutral, 1 tell, 2 swing, 3 recovery.
    pub fn set_player_attack_read(&mut self, phase: u8) {
        self.player_attack_read = phase;
    }
    pub fn combat_role(&self, index: usize) -> u8 {
        self.tactics.get(index).map_or(0, |t| t.combat_role)
    }
    pub fn observe_projectiles<const P: usize>(
        &mut self,
        records: &[LevelGameEntityRecord],
        projectiles: &crate::projectiles::CombatProjectiles<P>,
    ) {
        for (i, r) in records.iter().enumerate().take(self.count()) {
            self.projectile_threats[i] = projectiles.incoming_threat(
                crate::projectiles::CombatTeam::Enemy,
                r.room,
                self.position(i),
                i32::from(r.radius),
                i32::from(r.height),
            );
        }
    }
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn step_ranged_exchange(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
        delta: u16,
        far: i32,
        stats: &mut GameEntityTickStats,
    ) -> bool {
        let before = self.position(i);
        let energy = self.flow[i].energy;
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
        if order.fire {
            return false;
        }
        let goal = if order.evade {
            EnemyGoal::Evade
        } else if self.exchanges[i].mode == crate::ranged_tactics::PEEK {
            EnemyGoal::Peek
        } else {
            EnemyGoal::TakeCover
        };
        if self.tactics[i].goal != goal {
            self.begin_goal(i, goal, 240, 12);
        }
        self.release_attack_owner(i, 0);
        if !order.moving {
            self.face_tactical(i, input.player, delta);
            self.set_intent(i, GameEntityIntent::Hold);
            stats.holding = stats.holding.saturating_add(1);
            return true;
        }
        let facing_target = if order.evade {
            input.player
        } else {
            order.destination
        };
        let facing_error = self.face_tactical(i, facing_target, delta);
        if !order.evade && facing_error > 683 {
            return true;
        }
        let facing = self.yaw[i];
        self.tactics[i].running =
            !order.evade && Self::can_run(r) && Self::distance(before, order.destination) > 96;
        let speed = if order.evade {
            crate::combat_flow::EVADE_DISTANCE / i32::from(crate::combat_flow::EVADE_MOVE_TICKS)
        } else if self.tactics[i].running {
            r.run_speed.max(1)
        } else {
            r.walk_speed.max(1)
        };
        // Cover lanes have already been checked. Follow that segment directly;
        // the chase motor can retain its previous eight-way heading past a peek.
        let dx = order.destination[0] - before[0];
        let dz = order.destination[2] - before[2];
        let length = psx_math::int32::isqrt_i32(dx * dx + dz * dz).max(1);
        let step = (speed * i32::from(delta)).min(length);
        let p = mover.step(
            i,
            r.room,
            before,
            dx * step / length,
            dz * step / length,
            i32::from(r.radius),
            i32::from(r.height),
        );
        self.x[i] = p[0];
        self.y[i] = p[1];
        self.z[i] = p[2];
        self.yaw[i] = facing;
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
        true
    }
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn step_melee_defense(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
        delta: u16,
        stats: &mut GameEntityTickStats,
    ) -> bool {
        self.tactics[i].defend_cooldown = self.tactics[i].defend_cooldown.saturating_sub(delta);
        self.tactics[i].defend_ticks = self.tactics[i].defend_ticks.saturating_sub(delta);
        if !self.player_in_line_of_sight(r, i, input, mover) {
            return false;
        }
        let reach = Self::melee_attack_reach(r, input);
        if self.player_attack_read == 3 {
            if self.tactics[i].defend_ticks > 0 && self.tactics[i].goal == EnemyGoal::Retreat {
                self.finish_goal(i, EnemyGoalResult::Cancelled);
            }
            self.tactics[i].defend_ticks = 0;
            self.tactics[i].combat_role = 7;
            return false;
        }
        if self.player_attack_read == 1
            && self.tactics[i].defend_cooldown == 0
            && self.player_within(i, input, reach * 2)
        {
            self.tactics[i].defend_ticks = 24;
            self.tactics[i].defend_cooldown = 90;
        }
        if self.tactics[i].defend_ticks == 0 {
            return false;
        }
        self.tactics[i].combat_role = 6;
        self.release_attack_owner(i, 0);
        if self.tactics[i].goal != EnemyGoal::Retreat {
            self.begin_goal(i, EnemyGoal::Retreat, 24, reach + 32);
        }
        self.face_tactical(i, input.player, delta);
        if self.player_within(i, input, reach + 32) {
            let before = self.position(i);
            let yaw = psx_engine::Angle::from_q12(atan2_q12(
                before[0] - input.player[0],
                before[2] - input.player[2],
            ));
            let step = r.walk_speed.max(1) * i32::from(delta);
            let p = mover.step(
                i,
                r.room,
                before,
                yaw.sin().mul_i32(step),
                yaw.cos().mul_i32(step),
                i32::from(r.radius),
                i32::from(r.height),
            );
            self.x[i] = p[0];
            self.y[i] = p[1];
            self.z[i] = p[2];
        }
        self.set_intent(i, GameEntityIntent::Retreat);
        stats.retreating = stats.retreating.saturating_add(1);
        true
    }
    pub fn enable_combat_flow(&mut self, enabled: bool) {
        self.flow_enabled = enabled;
    }
    pub fn energy(&self, index: usize) -> u16 {
        self.flow.get(index).map_or(0, |f| f.energy)
    }
    pub fn can_fire_energy(&self, index: usize) -> bool {
        !self.flow_enabled || self.flow.get(index).is_some_and(|f| f.can_shoot())
    }
    pub fn projectile_connected(&mut self, index: usize, interrupted: bool) {
        if self.flow_enabled {
            if let Some(f) = self.flow.get_mut(index) {
                f.shot_hit(interrupted);
            }
        }
    }
    pub fn spend_shot_energy(&mut self, index: usize) {
        if self.flow_enabled {
            if let Some(f) = self.flow.get_mut(index) {
                f.spend_shot();
            }
        }
    }
    pub fn gain_melee_energy(&mut self, index: usize, heavy: bool) {
        if self.flow_enabled {
            if let Some(f) = self.flow.get_mut(index) {
                f.melee_hit(heavy);
            }
        }
    }
    pub fn shot_opening(&self, records: &[LevelGameEntityRecord], index: usize) -> bool {
        self.flow_enabled
            && index < self.count()
            && records.get(index).is_some_and(|r| {
                self.state(index) == GameEntityState::Windup
                    && self.state_ticks[index] >= u16::from(r.windup_ticks) / 2
                    && self.flow[index].can_interrupt()
            })
    }
    pub fn apply_projectile_hit(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        index: usize,
        channel: VitalityChannelId,
        damage: u16,
        poise: u16,
    ) -> GameEntityHitOutcome {
        if index >= self.count() || index >= records.len() {
            return GameEntityHitOutcome::MISS;
        }
        let poise = if self.flow_enabled {
            self.flow[index].shot_poise(
                poise,
                records[index].poise,
                self.shot_opening(records, index),
            )
        } else {
            poise
        };
        self.apply_stance_hit(records, index, channel, damage, poise)
    }
    /// A heavy connection creates separation through the same body collision as AI walking.
    pub fn recoil_from(&mut self, index: usize, from: [i32; 3]) {
        if !self.flow_enabled || index >= self.count() || self.state(index) == GameEntityState::Dead
        {
            return;
        }
        let yaw = psx_engine::Angle::from_q12(atan2_q12(
            self.x[index] - from[0],
            self.z[index] - from[2],
        ));
        self.recoil[index] = [yaw.sin().mul_i32(5) as i16, yaw.cos().mul_i32(5) as i16, 8];
    }
    pub(super) fn step_recoil(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        mover: &mut impl GameEntityMover,
        delta: u16,
    ) {
        let [dx, dz, ticks] = self.recoil[i];
        let step = delta.min(ticks.max(0) as u16);
        if step == 0 {
            return;
        }
        self.recoil[i][2] -= step as i16;
        let p = mover.step(
            i,
            r.room,
            self.position(i),
            i32::from(dx) * i32::from(step),
            i32::from(dz) * i32::from(step),
            i32::from(r.radius),
            i32::from(r.height),
        );
        self.x[i] = p[0];
        self.y[i] = p[1];
        self.z[i] = p[2];
    }
    /// Correct crowding during the first half of a melee tell, then plant.
    /// Collision and a fixed turn rate apply; attacks never track through impact.
    pub(super) fn step_melee_tell(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
        delta: u16,
    ) {
        if !self.flow_enabled || self.selected_attack_is_ranged(i) {
            return;
        }
        self.tactics[i].moved = false;
        self.tactics[i].running = false;
        if self.state_ticks[i] >= u16::from(r.windup_ticks) / 2
            || !self.player_in_line_of_sight(r, i, input, mover)
        {
            return;
        }
        self.face_tactical(i, input.player, delta);
        if !self.player_within(i, input, Self::melee_attack_reach(r, input) + 8) {
            return;
        }
        let before = self.position(i);
        let away = psx_engine::Angle::from_q12(atan2_q12(
            before[0] - input.player[0],
            before[2] - input.player[2],
        ));
        let step = r.walk_speed.max(1).saturating_mul(i32::from(delta));
        let p = mover.step(
            i,
            r.room,
            before,
            away.sin().mul_i32(step),
            away.cos().mul_i32(step),
            i32::from(r.radius),
            i32::from(r.height),
        );
        self.x[i] = p[0];
        self.y[i] = p[1];
        self.z[i] = p[2];
        self.tactics[i].goal = tactics::EnemyGoal::Retreat;
        self.advance_tactical_animation(r, i, before, delta);
    }
    pub fn firing_gait(&self, r: &LevelGameEntityRecord, i: usize) -> Option<(u16, u16)> {
        (self.flow_enabled
            && i < self.count()
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
        mover: &mut impl GameEntityMover,
        delta: u16,
    ) {
        if !self.flow_enabled || !Self::is_tactical(r) || !self.selected_attack_is_ranged(i) {
            return;
        }
        let before = self.position(i);
        self.tactics[i].moved = false;
        self.tactics[i].running = false;
        if !self.player_in_line_of_sight(r, i, input, mover) {
            return;
        }
        let dx = input.player[0] - before[0];
        let dz = input.player[2] - before[2];
        let toward = psx_engine::Angle::from_q12(atan2_q12(dx, dz));
        let (near, _) = crate::combat_flow::ranged_band(
            i32::from(r.preferred_distance),
            Self::melee_attack_reach(r, input) + 16,
            i32::from(r.attack_max_range),
        );
        let retreat = self.player_within(i, input, near);
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
        // Half normal walking speed while committing a shot; never sprint away.
        let budget = r.walk_speed.max(1) * i32::from(r.spacing_speed_percent.clamp(1, 100)) * 128
            / 100
            * i32::from(delta)
            + i32::from(self.tactics[i].movement_fraction_q8);
        let step = budget >> 8;
        self.tactics[i].movement_fraction_q8 = (budget & 255) as u8;
        if step > 0 {
            let p = mover.step(
                i,
                r.room,
                before,
                yaw.sin().mul_i32(step),
                yaw.cos().mul_i32(step),
                i32::from(r.radius),
                i32::from(r.height),
            );
            self.x[i] = p[0];
            self.y[i] = p[1];
            self.z[i] = p[2];
        }
        self.advance_tactical_animation(r, i, before, delta);
    }
}
