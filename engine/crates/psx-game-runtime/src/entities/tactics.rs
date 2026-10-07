//! Fixed-capacity tactical policy. Durations are 60 Hz simulation ticks, even
//! when NPC updates are batched. Research: bb-decomp movement-ai-policy,
//! movement-obstacle-recovery and movement-goal-lifecycle (October 2026).
//! Local steering reports physical progress; it never claims navmesh reachability.
use super::*;

/// Persistent movement objective, independent of the committed attack state.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnemyGoal {
    /// No active movement objective.
    None,
    /// Observe the target without translating.
    Hold,
    /// Close at walking speed, with a separate running threshold.
    Approach,
    /// Keep the selected side and face the target.
    CircleLeft,
    /// Keep the selected side and face the target.
    CircleRight,
    /// Walk backwards until the desired separation is reached.
    Retreat,
    /// Pause, then retry local steering after insufficient physical progress.
    WaitRetry,
    /// Try a bounded lateral destination after repeated blocked approaches.
    Reposition,
    /// Return to the authored spawn after loss of target or repeated blockage.
    ReturnHome,
    /// Turn and run to a firing distance before committing a ranged attack.
    BreakAway,
    /// Travel to geometry-verified cover.
    TakeCover,
    /// Step out of cover to a clear firing lane.
    Peek,
    /// Collision-bound evasive step; no invulnerability is granted.
    Evade,
}

/// Last objective outcome. Cancellation never masquerades as physical arrival.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnemyGoalResult {
    /// No completed objective yet.
    None,
    /// Requested separation or destination was physically reached.
    Arrived,
    /// The finite objective duration elapsed.
    Expired,
    /// Requested motion made insufficient physical progress.
    Blocked,
    /// Tactical state, damage or target ownership superseded this objective.
    Cancelled,
}

/// Read-only diagnostics for repeatable encounter tests and telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnemyTacticalSnapshot {
    /// Current objective.
    pub goal: EnemyGoal,
    /// Last completed objective's outcome.
    pub result: EnemyGoalResult,
    /// Monotonic wrapping transition counter; distinguish repeated same goals.
    pub generation: u16,
    /// Remaining objective lifetime in simulation ticks.
    pub remaining: u16,
    /// Running gait is explicitly selected, rather than inferred from clip availability.
    pub running: bool,
    /// Consecutive recovery attempts since useful movement.
    pub retries: u8,
    /// Time since the last unobstructed sight sample.
    pub unseen_ticks: u16,
    /// Accumulated physical travel during the current progress window.
    pub travel: u16,
    /// Shared policy reason for the last accepted stance switch.
    pub stance_reason: u8,
}

#[derive(Clone, Copy)]
pub(super) struct TacticalState {
    pub(super) goal: EnemyGoal,
    result: EnemyGoalResult,
    generation: u16,
    remaining: u16,
    phase: u16,
    pub(super) animation_phase_q8: u32,
    animation_clip: u16,
    pub(super) movement_fraction_q8: u8,
    motion_deferred: bool,
    separation: u16,
    sample_ticks: u16,
    travel: u16,
    requested: u16,
    start_distance: i32,
    recovery_distance: i32,
    pub(super) last_seen: [i32; 3],
    destination: [i32; 3],
    unseen_ticks: u16,
    retries: u8,
    pub(super) running: bool,
    pub(super) moved: bool,
    turning: bool,
    last_attack: u8,
    stance_reason: u8,
    repeat_count: u8,
    pub(super) seed: u32,
    pub(super) home_cooldown: u16,
    pub(super) defend_ticks: u16,
    pub(super) defend_cooldown: u16,
    pub(super) combat_role: u8,
}
impl TacticalState {
    pub(super) const EMPTY: Self = Self {
        goal: EnemyGoal::None,
        result: EnemyGoalResult::None,
        generation: 0,
        remaining: 0,
        phase: 0,
        animation_phase_q8: 0,
        animation_clip: u16::MAX,
        movement_fraction_q8: 0,
        motion_deferred: false,
        separation: 0,
        sample_ticks: 0,
        travel: 0,
        requested: 0,
        start_distance: 0,
        recovery_distance: i32::MAX,
        last_seen: [0; 3],
        destination: [0; 3],
        unseen_ticks: 0,
        retries: 0,
        running: false,
        moved: false,
        turning: false,
        last_attack: 255,
        stance_reason: 0,
        repeat_count: 0,
        seed: 0,
        home_cooldown: 0,
        defend_ticks: 0,
        defend_cooldown: 0,
        combat_role: 0,
    };
}

impl<const N: usize, const S: bool> GameEntities<N, S> {
    pub(super) fn is_tactical(record: &LevelGameEntityRecord) -> bool {
        record.flags & game_entity_flags::TACTICAL != 0
    }

    /// Relocate the one authored training actor for an obstacle fixture. This
    /// only accepts explicitly flagged encounter records and does not spawn an
    /// extra actor, grant attack ownership or manufacture a movement result.
    pub fn place_training_actor(
        &mut self,
        records: &[LevelGameEntityRecord],
        index: usize,
        position: [i32; 3],
        yaw: i16,
    ) -> bool {
        if index >= self.count()
            || records
                .get(index)
                .is_none_or(|r| r.flags & game_entity_flags::TRAINING == 0)
        {
            return false;
        }
        self.x[index] = position[0];
        self.y[index] = position[1];
        self.z[index] = position[2];
        self.yaw[index] = yaw;
        true
    }

    /// Let defeated actors finish their collapse while an encounter result stops AI and damage.
    pub fn advance_defeated_animations(&mut self, ticks: u16) {
        for index in 0..self.count() {
            if self.state(index)==GameEntityState::Dead {
                self.state_ticks[index]=self.state_ticks[index].saturating_add(ticks);
            }
        }
    }

    /// Committed attack family, for encounter diagnostics (light=0, heavy=1, ranged=2).
    pub fn attack_kind(&self, index: usize) -> u8 { self.selected_attack_kind(index) }

    /// Seed a flagged encounter without changing health, cooldowns or attack ownership.
    pub fn seed_training_actor(&mut self, records: &[LevelGameEntityRecord], index: usize, seed: u32) {
        if index < self.count() && records.get(index).is_some_and(|r| r.flags & game_entity_flags::TRAINING != 0) {
            self.tactics[index].seed = seed.max(1);
        }
    }

    pub(super) fn remember_target(&mut self, index: usize, position: [i32; 3]) {
        // Hearing supplies an acquisition location too. Subsequent hidden
        // movement must not track the player's live coordinates through walls.
        self.tactics[index].last_seen = position;
        self.tactics[index].unseen_ticks = 0;
    }

    /// Inspect tactical state without modifying the enemy or random stream.
    pub fn tactical_snapshot(&self, index: usize) -> EnemyTacticalSnapshot {
        let t = self.tactics[index];
        EnemyTacticalSnapshot {
            goal: t.goal,
            result: t.result,
            generation: t.generation,
            remaining: t.remaining,
            running: t.running,
            retries: t.retries,
            unseen_ticks: t.unseen_ticks,
            travel: t.travel,
            stance_reason: t.stance_reason,
        }
    }

    fn draw(&mut self, index: usize, limit: u16) -> u16 {
        let mut x = self.tactics[index].seed.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.tactics[index].seed = x;
        (x % u32::from(limit.max(1))) as u16
    }

    pub(super) fn finish_goal(&mut self, index: usize, result: EnemyGoalResult) {
        let t = &mut self.tactics[index];
        if t.goal != EnemyGoal::None {
            t.result = result;
            t.generation = t.generation.wrapping_add(1);
        }
        t.goal = EnemyGoal::None;
        t.remaining = 0;
        t.moved = false;
        t.running = false;
        t.turning = false;
    }

    pub(super) fn begin_goal(&mut self, index: usize, goal: EnemyGoal, ticks: u16, separation: i32) {
        self.finish_goal(index, EnemyGoalResult::Cancelled);
        let t = &mut self.tactics[index];
        t.goal = goal;
        t.remaining = ticks;
        t.separation = separation.clamp(0, 32767) as u16;
        t.phase = 0;
        t.sample_ticks = 0;
        t.travel = 0;
        t.requested = 0;
        t.start_distance = 0;
        t.generation = t.generation.wrapping_add(1);
        self.move_yaw_valid[index] = 0;
        self.move_tried[index] = 0;
    }

    pub(super) fn tactical_attack_ready(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
    ) -> bool {
        !Self::is_tactical(record)
            || (matches!(
                self.tactics[index].goal,
                EnemyGoal::None | EnemyGoal::Approach | EnemyGoal::Hold
            ) && self.tactics[index].unseen_ticks == 0)
    }

    pub(super) fn distance(a: [i32; 3], b: [i32; 3]) -> i32 {
        // Saturation keeps distant/invalid coordinates from overflowing the
        // Q12-era squared distance arithmetic. Chebyshev is sufficient for monitoring.
        a[0].saturating_sub(b[0])
            .saturating_abs()
            .max(a[2].saturating_sub(b[2]).saturating_abs())
    }

    pub(super) fn face_tactical(&mut self, index: usize, target: [i32; 3], delta: u16) -> u16 {
        let dx = target[0].saturating_sub(self.x[index]);
        let dz = target[2].saturating_sub(self.z[index]);
        if dx == 0 && dz == 0 {
            return 0;
        }
        let target = psx_engine::Angle::from_q12(atan2_q12(dx, dz));
        let current = psx_engine::Angle::from_q12(self.yaw[index] as u16);
        // 180 degrees/sec: fast enough to engage, visibly planted when flanked.
        let next = current.approach_q12(target, 34u16.saturating_mul(delta));
        self.yaw[index] = next.as_q12() as i16;
        let d = target.as_q12().wrapping_sub(next.as_q12()) & 4095;
        let error = d.min(4096 - d);
        self.tactics[index].turning = error > 57;
        error
    }

    pub(super) fn tactical_clip(
        &self,
        r: &LevelGameEntityRecord,
        i: usize,
    ) -> Option<GameEntityClip> {
        if self.state(i) != GameEntityState::Aggro
            || self.state_ticks[i] < u16::from(r.reaction_ticks)
        {
            return None;
        }
        Some(GameEntityClip {
            clip: self.tactical_locomotion_clip(r, i),
            phase_ticks: (self.tactics[i].animation_phase_q8 >> 8) as u16,
            one_shot: false,
            speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            frame_range: CharacterActionFrameRange::FULL,
        })
    }

    pub(super) fn tactical_locomotion_clip(&self, r: &LevelGameEntityRecord, i: usize) -> u16 {
        let t = self.tactics[i];
        if !t.moved {
            if t.turning {
                r.turn_clip
            } else {
                r.idle_clip
            }
        } else {
            match t.goal {
                EnemyGoal::Evade => if self.intent(i) == GameEntityIntent::CircleLeft {
                    r.strafe_left_clip
                } else { r.strafe_right_clip },
                EnemyGoal::CircleLeft => r.strafe_left_clip,
                EnemyGoal::CircleRight => r.strafe_right_clip,
                EnemyGoal::Retreat => r.walk_backward_clip,
                _ if t.running => r.run_clip,
                _ => r.walk_clip,
            }
        }
    }

    fn tactical_walk_speed_q8(r: &LevelGameEntityRecord, goal: EnemyGoal) -> i32 {
        let speed = r.walk_speed.max(1).saturating_mul(256);
        if matches!(goal, EnemyGoal::CircleLeft | EnemyGoal::CircleRight | EnemyGoal::Retreat) {
            (speed.saturating_mul(i32::from(r.spacing_speed_percent.clamp(1, 100))) / 100).max(1)
        } else {
            speed
        }
    }

    fn tactical_walk_speed(r: &LevelGameEntityRecord, goal: EnemyGoal) -> i32 {
        ((Self::tactical_walk_speed_q8(r, goal) + 255) >> 8).max(1)
    }

    pub(super) fn advance_tactical_animation(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        before: [i32; 3],
        delta: u16,
    ) {
        if self.state(i) != GameEntityState::Aggro && !(self.flow_enabled && (self.selected_attack_is_ranged(i) || self.state(i)==GameEntityState::Windup)
            && matches!(self.state(i),GameEntityState::Windup|GameEntityState::Attack|GameEntityState::Recover)) {
            self.tactics[i].animation_clip = u16::MAX;
            return;
        }
        let dx = self.x[i].saturating_sub(before[0]).clamp(-64, 64);
        let dz = self.z[i].saturating_sub(before[2]).clamp(-64, 64);
        self.tactics[i].moved = dx != 0 || dz != 0
            || (self.tactics[i].motion_deferred && self.tactics[i].moved);
        let clip = self.tactical_locomotion_clip(r, i);
        // Objective lifetimes and animation clocks are independent. Replanning
        // the same gait must not snap both feet back to their first frame.
        if self.tactics[i].animation_clip != clip {
            self.tactics[i].animation_clip = clip;
            self.tactics[i].animation_phase_q8 = 0;
        }
        let advance = if self.tactics[i].moved {
            let travel_q8 = psx_math::int32::isqrt_i32((dx * dx + dz * dz) << 16);
            let nominal_speed_q8 = if self.tactics[i].running {
                r.run_speed.max(1).saturating_mul(256)
            } else {
                Self::tactical_walk_speed_q8(r, self.tactics[i].goal)
            };
            ((travel_q8 << 8) / nominal_speed_q8.max(1)) as u32
        } else {
            u32::from(delta) * 256
        };
        self.tactics[i].animation_phase_q8 =
            self.tactics[i].animation_phase_q8.wrapping_add(advance) & 0x00ff_ffff;
    }

    pub(super) fn choose_spacing(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
    ) {
        let p = i32::from(r.preferred_distance).max(Self::melee_attack_reach(r, input));
        let roll = self.draw(i, 100);
        // Adapt the observed near retreat / middle circle / far approach bands
        // to the authored Cortex combat radius. Re-sample AFTER the recovery window.
        let (goal, separation, base, spread) = if self.player_within(i, input, p) {
            if roll < 70 {
                (EnemyGoal::Retreat, p + p / 2, 180, 91)
            } else if roll < 90 {
                (self.circle_side(i), p + p / 2, 210, 91)
            } else {
                (EnemyGoal::Hold, p, 36, 25)
            }
        } else if self.player_within(i, input, p * 3) {
            if roll < u16::from(r.circle_chance) {
                (self.circle_side(i), p + p / 2, 210, 91)
            } else if roll < 85 {
                (EnemyGoal::Approach, p, 180, 91)
            } else {
                (EnemyGoal::Hold, p, 36, 25)
            }
        } else {
            (EnemyGoal::Approach, p, 240, 61)
        };
        let duration = if self.flow_enabled {
            // A readable half-to-one-second reset, not a multi-second refusal
            // to consider another attack while the player recovers for free.
            36 + self.draw(i, 37)
        } else { base + self.draw(i, spread) };
        self.begin_goal(i, goal, duration, separation);
        // The director must let the spacing goal finish before granting another swing.
        if goal == EnemyGoal::Hold {
            self.attack_cooldown[i] = self.attack_cooldown[i].max(duration);
        }
    }

    fn circle_side(&mut self, i: usize) -> EnemyGoal {
        if self.draw(i, 2) == 0 {
            EnemyGoal::CircleLeft
        } else {
            EnemyGoal::CircleRight
        }
    }

    fn choose_attack(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        ranged: bool,
    ) {
        let close = self.player_within(i, input, Self::melee_attack_reach(r, input) * 3 / 4);
        let roll = if ranged || self.tactics[i].repeat_count >= 2 { 0 } else { self.draw(i, 100) };
        let kind = crate::combat_policy::attack_choice(ranged, close,
            self.tactics[i].last_attack, self.tactics[i].repeat_count, roll);
        let t = &mut self.tactics[i];
        t.repeat_count = if t.last_attack == kind {
            t.repeat_count.saturating_add(1)
        } else {
            1
        };
        t.last_attack = kind;
        self.attack_mode[i] = kind;
    }

    fn go_home(&mut self, r: &LevelGameEntityRecord, i: usize) {
        self.release_attack_owner(i, u16::from(r.group_attack_delay_ticks));
        self.begin_goal(i, EnemyGoal::ReturnHome, 6000, i32::from(r.radius).max(2));
        self.tactics[i].destination = [r.x, r.y, r.z];
        self.tactics[i].recovery_distance = Self::distance(self.position(i), [r.x, r.y, r.z]);
    }

    /// Physical travel and destination progress are independent. Circling should
    /// travel without closing; an approach orbiting a wall should eventually retry.
    fn monitor_progress(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        before: [i32; 3],
        target: [i32; 3],
        requested: i32,
        delta: u16,
    ) {
        let after = self.position(i);
        let distance = Self::distance(after, target);
        let t = &mut self.tactics[i];
        if t.sample_ticks == 0 {
            t.start_distance = Self::distance(before, target);
        }
        t.sample_ticks = t.sample_ticks.saturating_add(delta);
        t.travel = t
            .travel
            .saturating_add(Self::distance(before, after).min(32767) as u16);
        t.requested = t.requested.saturating_add(requested.clamp(0, 32767) as u16);
        if t.sample_ticks < 60 {
            return;
        }
        let no_travel = t.requested > 0 && t.travel < (t.requested / 8).max(2);
        let no_progress = matches!(
            t.goal,
            EnemyGoal::Approach | EnemyGoal::ReturnHome | EnemyGoal::Reposition
        ) && t.travel > 0
            && distance >= t.start_distance
            && t.sample_ticks >= 180;
        if no_travel || no_progress {
            if self.flow_enabled && t.goal == EnemyGoal::BreakAway {
                self.flow[i].contest_space();
                self.finish_goal(i, EnemyGoalResult::Blocked);
                self.release_attack_owner(i, 0);
                return;
            }
            let home = t.goal == EnemyGoal::ReturnHome;
            if t.retries == 0 {
                t.recovery_distance = Self::distance(after, if home { target } else { t.last_seen });
            }
            let retries = t.retries.saturating_add(1);
            self.finish_goal(i, EnemyGoalResult::Blocked);
            self.release_attack_owner(i, 0);
            self.begin_goal(i, EnemyGoal::WaitRetry, 120, 0);
            self.tactics[i].retries = retries;
            // Returning home can also be obstructed. Retain a bounded retry,
            // then cool off at the actual location; never teleport through geometry.
            if home && retries >= 4 {
                self.tactics[i].home_cooldown = 180;
            }
        } else if t.sample_ticks >= 180 || distance + i32::from(r.radius) < t.start_distance {
            if matches!(t.goal, EnemyGoal::Approach | EnemyGoal::ReturnHome)
                && distance + i32::from(r.radius) < t.recovery_distance
            {
                // A lateral detour followed by walking back to the same wall
                // is not recovered reachability. Beat the pre-retry distance.
                t.retries = 0;
                t.recovery_distance = distance;
            }
            t.sample_ticks = 0;
            t.travel = 0;
            t.requested = 0;
        }
    }

    pub(super) fn tick_tactical(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
        delta: u16,
        stats: &mut GameEntityTickStats,
    ) {
        let before = self.position(i);
        let was_moving = self.tactics[i].moved;
        self.tactics[i].motion_deferred = false;
        self.tick_tactical_policy(r, i, input, mover, delta, stats);
        if self.tactics[i].motion_deferred {
            self.tactics[i].moved = was_moving;
        }
        self.advance_tactical_animation(r, i, before, delta);
    }

    // Keep low-frequency AI decisions compact on the PS1; rendering stays at opt-level 2.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn tick_tactical_policy(
        &mut self,
        r: &LevelGameEntityRecord,
        i: usize,
        input: GameEntityTickInput<'_>,
        mover: &mut impl GameEntityMover,
        delta: u16,
        stats: &mut GameEntityTickStats,
    ) {
        self.tactics[i].moved = false;
        self.tactics[i].turning = false;
        self.tactics[i].phase = self.tactics[i].phase.wrapping_add(delta);
        let home = [r.x, r.y, r.z];
        let inside_leash = input.player_room == r.room
            && within_xz(
                [r.x, r.z],
                [input.player[0], input.player[2]],
                i32::from(r.aggro_radius) * 2,
            );
        let visible =
            input.player_room == r.room && self.player_in_line_of_sight(r, i, input, mover);
        if visible {
            self.tactics[i].last_seen = input.player;
            self.tactics[i].unseen_ticks = 0;
        } else {
            self.tactics[i].unseen_ticks = self.tactics[i].unseen_ticks.saturating_add(delta);
        }
        if self.state_ticks[i] < u16::from(r.reaction_ticks) {
            self.face_tactical(i, self.tactics[i].last_seen, delta);
            return;
        }
        if (!inside_leash || self.tactics[i].unseen_ticks >= 240)
            && !matches!(
                self.tactics[i].goal,
                EnemyGoal::ReturnHome | EnemyGoal::WaitRetry
            )
        {
            self.go_home(r, i);
        }
        let mut goal = self.tactics[i].goal;
        if goal == EnemyGoal::ReturnHome {
            if within_xz(
                [self.x[i], self.z[i]],
                [r.x, r.z],
                i32::from(r.radius).max(2),
            ) && self.y[i].saturating_sub(r.y).saturating_abs() <= i32::from(r.height) / 2
            {
                self.finish_goal(i, EnemyGoalResult::Arrived);
                self.tactics[i].home_cooldown = 120;
                self.enter_state(i, GameEntityState::Idle, stats);
                return;
            }
            // A nearby, visible threat can interrupt returning home, without
            // recording false arrival; death and stagger use the same cancellation contract.
            if visible
                && inside_leash
                && self.player_within(i, input, Self::melee_attack_reach(r, input))
            {
                self.finish_goal(i, EnemyGoalResult::Cancelled);
                goal = EnemyGoal::None;
            }
        }
        let previous = self.tactics[i].remaining;
        self.tactics[i].remaining = previous.saturating_sub(delta);
        if goal == EnemyGoal::WaitRetry {
            self.set_intent(i, GameEntityIntent::Hold);
            self.face_tactical(i, self.tactics[i].last_seen, delta);
            // Policy-2-like two-second wait, with one-second local movement retry.
            if previous > 60 && self.tactics[i].remaining <= 60 && visible {
                let before = self.position(i);
                let heading = psx_engine::Angle::from_q12(atan2_q12(
                    input.player[0].saturating_sub(before[0]),
                    input.player[2].saturating_sub(before[2]),
                ));
                let step = r.walk_speed.max(1);
                let committed = mover.step_direction(
                    i,
                    r.room,
                    before,
                    Self::q12_step_component(heading.sin().raw(), step),
                    Self::q12_step_component(heading.cos().raw(), step),
                    i32::from(r.radius),
                    i32::from(r.height),
                );
                self.x[i] = committed[0];
                self.y[i] = committed[1];
                self.z[i] = committed[2];
                // Sliding sideways inside an enclosure is movement, but is
                // not evidence that the approach can resume. Do not erase
                // repeated failure just because a fallback direction moved.
                if self.tactics[i].retries < 3
                    && Self::distance(committed, input.player)
                        < Self::distance(before, input.player)
                {
                    self.finish_goal(i, EnemyGoalResult::Cancelled);
                    self.begin_goal(
                        i,
                        EnemyGoal::Approach,
                        240,
                        Self::melee_attack_reach(r, input),
                    );
                    return;
                }
            }
            if self.tactics[i].remaining != 0 {
                return;
            }
            self.finish_goal(i, EnemyGoalResult::Expired);
            if self.tactics[i].home_cooldown != 0 {
                self.enter_state(i, GameEntityState::Idle, stats);
                return;
            }
            if self.tactics[i].retries >= 3
                || !inside_leash
                || self.tactics[i].unseen_ticks >= 240
            {
                self.go_home(r, i);
            } else {
                let left = self.draw(i, 2) == 0;
                let p = i32::from(r.preferred_distance).max(i32::from(r.radius) * 3);
                let yaw = psx_engine::Angle::from_q12(self.yaw[i] as u16)
                    .add(psx_engine::Angle::from_q12(if left { 1024 } else { 3072 }));
                self.begin_goal(i, EnemyGoal::Reposition, 180, i32::from(r.radius));
                self.tactics[i].destination = [
                    self.x[i] + yaw.sin().mul_i32(p),
                    self.y[i],
                    self.z[i] + yaw.cos().mul_i32(p),
                ];
            }
            return;
        }
        if goal != EnemyGoal::None && previous > 0 && self.tactics[i].remaining == 0 {
            if self.flow_enabled && goal == EnemyGoal::BreakAway && visible
                && self.player_within(i, input, Self::melee_attack_reach(r, input) * 3) {
                self.flow[i].contest_space();
            }
            self.finish_goal(i, EnemyGoalResult::Expired);
            if goal == EnemyGoal::ReturnHome {
                self.tactics[i].home_cooldown = 180;
                self.enter_state(i, GameEntityState::Idle, stats);
                return;
            }
            goal = EnemyGoal::None;
        }
        let target = if visible {
            input.player
        } else {
            self.tactics[i].last_seen
        };
        let input = GameEntityTickInput {
            player: target,
            ..input
        };
        // A wider spacing orbit must not trap a hybrid in an endless cannon
        // loop. After two shots, commit to closing when the target is nearby;
        // the existing stance cooldown still gates when the switch can happen.
        let melee_height_reachable = (input.player[1] - self.y[i]).abs() <= i32::from(r.height);
        let press_after_cannon = visible
            && melee_height_reachable
            && self.tactics[i].last_attack == GAME_ENTITY_ATTACK_RANGED
            && self.tactics[i].repeat_count >= 2
            && self.player_within(i, input, i32::from(r.preferred_distance) * 3);
        let melee = melee_height_reachable && (self.update_melee_chase(r, i, input) || press_after_cannon);
        let separated = !self.player_within(i, input, Self::melee_attack_reach(r, input) * 2);
        // Reconsider resource/height changes during spacing, but preserve physical
        // obstacle recovery goals and every committed attack animation.
        let can_replan = matches!(goal, EnemyGoal::None | EnemyGoal::Approach | EnemyGoal::Hold
            | EnemyGoal::TakeCover | EnemyGoal::Peek | EnemyGoal::Evade)
            || (self.flow_enabled && matches!(goal, EnemyGoal::CircleLeft | EnemyGoal::CircleRight | EnemyGoal::Retreat | EnemyGoal::BreakAway)
                && self.tactics[i].phase >= 24);
        // Free locomotion can evade in either stance, including with an empty
        // Energy reserve. Windup/attack/recovery never enter this policy.
        if self.flow_enabled && !self.stance_swap_in_progress(i)
            && self.exchanges[i].try_evade(self.position(i), i32::from(r.radius),
                i32::from(r.height), self.projectile_threats[i],
                &mut |a,b| mover.line_of_sight(r.room,a,b)) {
            self.step_ranged_exchange(r,i,input,mover,delta,i32::from(r.attack_max_range),stats);
            return;
        }
        if S && can_replan {
            let (stance, reason) = crate::combat_policy::stance_choice(melee, self.stance(i),
                [self.health[i], self.health_secondary[i]], [r.max_health, r.max_health_secondary],
                press_after_cannon, if visible { input.player_combat } else {None});
            let mut stance_reason = reason as u8;
            let mut desired = stance;
            if self.flow_enabled {
                desired = if self.flow[i].preferred_stance(stance.index() as u8, separated) == 0 {
                    VitalityChannelId::One } else { VitalityChannelId::Two };
                stance_reason = self.flow[i].stance_reason(stance_reason, separated);
                let (near, _) = crate::combat_flow::ranged_band(
                    i32::from(r.preferred_distance), Self::melee_attack_reach(r, input) + 16,
                    i32::from(r.attack_max_range));
                if desired == VitalityChannelId::Two && self.stance(i) == VitalityChannelId::One
                    && Self::distance(self.position(i), home) >= i32::from(r.aggro_radius)
                    && self.player_within(i, input, near) {
                    // Stay ready to defend at the boundary. Swapping out and back
                    // cannot create an escape route that does not exist.
                    desired = VitalityChannelId::One;
                    stance_reason = 10;
                }
            }
            // An overhead target cannot replenish a ground actor's Energy.
            // Keep the cannon selected while empty and wait for a reachable target.
            if !melee_height_reachable && Self::has_ranged_attack(r) {
                desired = VitalityChannelId::Two;
                stance_reason = 8;
            }
            if desired != self.stance(i) && self.stance_swap_cooldown[i] == 0 && visible {
                self.mutate_stance(i);
                if self.stance(i) == desired {
                    self.tactics[i].stance_reason = stance_reason;
                    if self.flow_enabled {
                        self.finish_goal(i, EnemyGoalResult::Cancelled);
                        goal = EnemyGoal::None;
                    }
                }
            }
            if self.stance_swap_in_progress(i) {
                self.face_tactical(i, target, delta);
                return;
            }
        }
        let melee = if S {
            self.stance(i) == VitalityChannelId::One || !Self::has_ranged_attack(r)
        } else {
            melee
        };
        let reach = if melee {
            Self::melee_attack_reach(r, input) + if self.flow_enabled { 16 } else { 0 }
        } else {
            Self::attack_reach(r, input)
        };
        let p = i32::from(r.preferred_distance).max(Self::melee_attack_reach(r, input));
        let (fire_near, fire_far) = crate::combat_flow::ranged_band(p,
            Self::melee_attack_reach(r, input) + 16, i32::from(r.attack_max_range));
        if self.flow_enabled {
            self.tactics[i].combat_role = 0;
            if melee {
                self.exchanges[i].reset();
                if self.step_melee_defense(r,i,input,mover,delta,stats) { return; }
                goal = self.tactics[i].goal;
            } else if self.can_fire_energy(i) && !self.player_within(i,input,Self::melee_attack_reach(r,input)*2) {
                if self.step_ranged_exchange(r,i,input,mover,delta,fire_far,stats) { return; }
            }
            if matches!(goal, EnemyGoal::TakeCover | EnemyGoal::Peek | EnemyGoal::Evade) {
                self.finish_goal(i,EnemyGoalResult::Cancelled);
                goal = EnemyGoal::None;
            }
        }
        if self.flow_enabled && melee && goal == EnemyGoal::BreakAway {
            self.finish_goal(i, EnemyGoalResult::Cancelled);
            goal = EnemyGoal::None;
        }
        if self.flow_enabled && !melee && visible && self.can_fire_energy(i)
            && self.player_within(i, input, fire_near)
            && matches!(goal, EnemyGoal::None | EnemyGoal::Approach | EnemyGoal::Hold
                | EnemyGoal::CircleLeft | EnemyGoal::CircleRight | EnemyGoal::Retreat) {
            self.release_attack_owner(i, 0);
            self.begin_goal(i, EnemyGoal::BreakAway, 180, fire_far);
            goal = EnemyGoal::BreakAway;
        }
        // A stance-locked gunner must keep a real firing band. Otherwise a
        // melee approach repeatedly crosses the projectile minimum and backs
        // out again while the stance cooldown is still closed.
        if melee && self.flow_enabled && goal == EnemyGoal::Approach {
            self.tactics[i].separation = reach as u16;
        }
        if !melee && goal == EnemyGoal::Approach {
            self.tactics[i].separation = (if self.flow_enabled { fire_far } else { p * 2 })
                .min(reach)
                .max(i32::from(r.attack_min_range) + i32::from(r.spacing_tolerance))
                as u16;
        }
        if goal == EnemyGoal::None {
            // A short assessment before approaching makes initial acquisition and
            // post-goal replans readable; circling side is drawn once per objective.
            let separation = if !melee || (Self::has_ranged_attack(r) && self.draw(i, 100) < 30) {
                (p * 2).min(i32::from(r.attack_max_range))
            } else {
                Self::melee_attack_reach(r, input)
            };
            let separation = if self.flow_enabled { if melee { reach } else { fire_far } } else { separation };
            self.begin_goal(i, EnemyGoal::Approach, 300, separation);
            goal = EnemyGoal::Approach;
        }
        let error = if matches!(goal, EnemyGoal::ReturnHome | EnemyGoal::Reposition | EnemyGoal::BreakAway) {
            0
        } else {
            self.face_tactical(i, target, delta)
        };
        if visible
            && goal != EnemyGoal::BreakAway
            && (!melee || melee_height_reachable)
            && self.attack_owner() == Some(i)
            && self.tactical_attack_ready(r, i)
            && (melee || self.can_fire_energy(i))
            && self.player_within(
                i,
                input,
                reach.min(
                    i32::from(self.tactics[i].separation).max(Self::melee_attack_reach(r, input)),
                ),
            )
            && error <= 341
            && (melee || !self.player_within(i, input,
                if self.flow_enabled { fire_near } else { i32::from(r.attack_min_range) }))
        {
            self.choose_attack(r, i, input, !melee);
            if !melee {
                self.capture_ranged_aim(i, input);
            }
            self.finish_goal(i, EnemyGoalResult::Arrived);
            self.enter_state(i, GameEntityState::Windup, stats);
            return;
        }
        if !melee
            && visible
            && self.player_within(i, input, i32::from(r.attack_min_range))
            && !matches!(
                goal,
                EnemyGoal::ReturnHome | EnemyGoal::Reposition | EnemyGoal::Retreat | EnemyGoal::BreakAway
            )
        {
            self.release_attack_owner(i, 0);
            self.begin_goal(i, EnemyGoal::Retreat, 210, p);
            goal = EnemyGoal::Retreat;
        }
        if goal == EnemyGoal::Hold {
            self.set_intent(i, GameEntityIntent::Hold);
            stats.holding = stats.holding.saturating_add(1);
            return;
        }
        let before = self.position(i);
        let old_yaw = self.yaw[i];
        let mut destination = target;
        let mut running = false;
        let separation = i32::from(self.tactics[i].separation);
        let mut speed = Self::tactical_walk_speed(r, goal);
        match goal {
            EnemyGoal::Approach => {
                let far = p * 5;
                let near = p * 3;
                running = Self::can_run(r)
                    && !self.player_within(
                        i,
                        input,
                        if self.tactics[i].running { near } else { far },
                    );
                if running {
                    speed = r.run_speed.max(1);
                }
                self.set_intent(i, GameEntityIntent::Approach);
                if visible && self.player_within(i, input, separation) {
                    if self.flow_enabled && self.attack_cooldown[i] == 0 {
                        // Keep the settled goal while turning/acquiring the attack
                        // token instead of restarting it on every policy tick.
                        self.set_intent(i, GameEntityIntent::Hold);
                        return;
                    }
                    self.finish_goal(i, EnemyGoalResult::Arrived);
                    if self.attack_cooldown[i] != 0 {
                        self.choose_spacing(r, i, input);
                    }
                    return;
                }
                if error > 683 {
                    return;
                }
            }
            EnemyGoal::CircleLeft | EnemyGoal::CircleRight => {
                if !visible || !self.player_within(i, input, p * 4) {
                    self.finish_goal(i, EnemyGoalResult::Cancelled);
                    return;
                }
                let offset = if goal == EnemyGoal::CircleLeft {
                    3072
                } else {
                    1024
                };
                let yaw = psx_engine::Angle::from_q12(atan2_q12(
                    target[0] - before[0],
                    target[2] - before[2],
                ))
                .add(psx_engine::Angle::from_q12(offset));
                destination = [
                    before[0] + yaw.sin().mul_i32(speed * i32::from(delta) * 2),
                    before[1],
                    before[2] + yaw.cos().mul_i32(speed * i32::from(delta) * 2),
                ];
                self.set_intent(
                    i,
                    if goal == EnemyGoal::CircleLeft {
                        GameEntityIntent::CircleLeft
                    } else {
                        GameEntityIntent::CircleRight
                    },
                );
                stats.circling = stats.circling.saturating_add(1);
            }
            EnemyGoal::BreakAway => {
                if !visible {
                    self.finish_goal(i, EnemyGoalResult::Cancelled);
                    return;
                }
                if !self.player_within(i, input, separation) {
                    self.finish_goal(i, EnemyGoalResult::Arrived);
                    return;
                }
                // Do not kite a live opponent past the home leash and then
                // attempt to walk through them to reset. Contest the space here.
                if Self::distance(before, home) >= i32::from(r.aggro_radius) {
                    self.flow[i].contest_space();
                    self.finish_goal(i, EnemyGoalResult::Cancelled);
                    return;
                }
                destination = [before[0].saturating_add(before[0].saturating_sub(target[0])),
                    before[1], before[2].saturating_add(before[2].saturating_sub(target[2]))];
                self.set_intent(i, GameEntityIntent::Retreat);
                // Turning and running are visible, collision-bound and vulnerable.
                if self.face_tactical(i, destination, delta) > 683 { return; }
                running = Self::can_run(r);
                speed = if running { r.run_speed.max(1) } else { r.walk_speed.max(1) };
                stats.retreating = stats.retreating.saturating_add(1);
            }
            EnemyGoal::Retreat => {
                if !visible {
                    self.finish_goal(i, EnemyGoalResult::Cancelled);
                    return;
                }
                if !self.player_within(i, input, separation) {
                    self.finish_goal(i, EnemyGoalResult::Arrived);
                    return;
                }
                destination = [
                    before[0].saturating_add(before[0].saturating_sub(target[0])),
                    before[1],
                    before[2].saturating_add(before[2].saturating_sub(target[2])),
                ];
                self.set_intent(i, GameEntityIntent::Retreat);
                stats.retreating = stats.retreating.saturating_add(1);
            }
            EnemyGoal::ReturnHome | EnemyGoal::Reposition => {
                destination = if goal == EnemyGoal::ReturnHome {
                    home
                } else {
                    self.tactics[i].destination
                };
                self.set_intent(i, GameEntityIntent::Approach);
                if Self::distance(before, destination) <= separation
                    && goal == EnemyGoal::Reposition
                {
                    self.finish_goal(i, EnemyGoalResult::Arrived);
                    return;
                }
                if self.face_tactical(i, destination, delta) > 683 {
                    return;
                }
            }
            _ => return,
        }
        if self.tactics[i].running != running {
            self.tactics[i].phase = 0;
        }
        self.tactics[i].running = running;
        let facing = self.yaw[i];
        // Refresh steering every 0.3 s at either NPC cadence. The legacy
        // state_tick modulo-four choice can alias when delta is two.
        if self.tactics[i].phase % 18 < delta {
            self.move_yaw_valid[i] = 0;
        }
        let mut requested_step = speed.saturating_mul(i32::from(delta));
        if matches!(
            goal,
            EnemyGoal::CircleLeft | EnemyGoal::CircleRight | EnemyGoal::Retreat
        ) {
            let yaw = psx_engine::Angle::from_q12(atan2_q12(
                destination[0] - before[0],
                destination[2] - before[2],
            ));
            // Keep sub-unit spacing speeds without forcing a one-unit move
            // on every 60 Hz update. The remainder also survives batched ticks.
            let budget_q8 = Self::tactical_walk_speed_q8(r, goal)
                .saturating_mul(i32::from(delta))
                .saturating_add(i32::from(self.tactics[i].movement_fraction_q8));
            let step = budget_q8 >> 8;
            requested_step = step;
            self.tactics[i].movement_fraction_q8 = (budget_q8 & 255) as u8;
            if step == 0 {
                self.tactics[i].motion_deferred = true;
                return;
            }
            let mut dx = Self::q12_step_component(yaw.sin().raw(), step);
            let mut dz = Self::q12_step_component(yaw.cos().raw(), step);
            if matches!(goal, EnemyGoal::CircleLeft | EnemyGoal::CircleRight) {
                let radial = psx_engine::Angle::from_q12(atan2_q12(
                    target[0] - before[0],
                    target[2] - before[2],
                ));
                let correction =
                    if self.player_within(i, input, separation - i32::from(r.spacing_tolerance)) {
                        -1
                    } else if !self.player_within(
                        i,
                        input,
                        separation + i32::from(r.spacing_tolerance),
                    ) {
                        1
                    } else {
                        0
                    };
                dx += radial.sin().mul_i32(step / 2) * correction;
                dz += radial.cos().mul_i32(step / 2) * correction;
            }
            let committed = mover.step(
                i,
                r.room,
                before,
                dx,
                dz,
                i32::from(r.radius),
                i32::from(r.height),
            );
            self.commit_step(i, before, committed);
        } else {
            self.step_toward(
                r,
                i,
                destination,
                speed.saturating_mul(i32::from(delta)),
                mover,
            );
        }
        // Steering commits position, while the tactical policy owns bounded facing.
        self.yaw[i] = if matches!(goal, EnemyGoal::ReturnHome | EnemyGoal::Reposition | EnemyGoal::BreakAway) {
            facing
        } else {
            old_yaw
        };
        self.tactics[i].moved = Self::distance(before, self.position(i)) > 0;
        self.monitor_progress(
            r,
            i,
            before,
            destination,
            requested_step,
            delta,
        );
    }
}

#[cfg(test)]
mod tests;
