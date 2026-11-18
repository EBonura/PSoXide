extern crate std;
use super::*;
use crate::combat_intent::CombatIntent;
use crate::entities::tests::StepRule;
use crate::entities::tests::{test_record, BODY_SUB_UNIT, BODY_TACTICAL, TEST_BODIES};
use psx_engine::SimTick;

const fn enemy() -> LevelGameEntityRecord {
    let mut r = test_record(
        0,
        0,
        0,
        400,
        game_entity_flags::ENABLED
            | game_entity_flags::CAN_RUN
            | game_entity_flags::RANGED_ATTACK
            | game_entity_flags::MELEE_ATTACK
            | game_entity_flags::LIGHT_ATTACK
            | game_entity_flags::HEAVY_ATTACK,
    );
    r.character = psx_level::CharacterIndex(BODY_TACTICAL);
    r.preferred_distance = 64;
    r.spacing_tolerance = 12;
    r.attack_min_range = 32;
    r.attack_max_range = 256;
    r.circle_chance = 65;
    r.reaction_ticks = 42;
    r.attack_cooldown_ticks = 45;
    r.windup_ticks = 16;
    r.attack_active_ticks = 59;
    r.heavy_attack_active_ticks = 96;
    r.ranged_attack_active_ticks = 148;
    r.recovery_ticks = 24;
    r
}

/// The test bodies with the tactical enemy's combat numbers changed by
/// `change`, for a fighter set to read them ([`GameEntities::set_characters`]).
fn tactical_bodies(
    change: impl FnOnce(&mut psx_level::CombatStats),
) -> &'static [LevelCharacterRecord] {
    let mut bodies = TEST_BODIES;
    change(&mut bodies[usize::from(BODY_TACTICAL)].combat);
    std::boxed::Box::leak(std::boxed::Box::new(bodies))
}
static ENEMY: [LevelGameEntityRecord; 1] = [enemy()];
fn input(p: [i32; 3]) -> GameEntityTickInput<'static> {
    GameEntityTickInput {
        target: CombatTarget {
            position: p,
            radius: 12,
            height: 64,
            noise_radius: 100,
            defender: crate::combatant::Defender::OPEN,
            stance: None,
            attack_read: 0,
            swing_ticks: None,
        },
    }
}
#[derive(Default)]
struct Arena {
    blocked: bool,
    hidden: bool,
    wall: bool,
    steps: u32,
}
impl StepRule for Arena {
    fn step(
        &mut self,
        _: usize,
        _: RoomIndex,
        p: [i32; 3],
        dx: i32,
        dz: i32,
        _: i32,
        _: i32,
    ) -> [i32; 3] {
        self.steps += 1;
        let q = [p[0] + dx, p[1], p[2] + dz];
        if self.blocked || (self.wall && q[2] > 110 && q[2] < 150 && q[0].abs() < 50) {
            p
        } else {
            q
        }
    }
    fn line_of_sight(&mut self, _: RoomIndex, _: [i32; 3], _: [i32; 3]) -> bool {
        !self.hidden
    }
}
fn engaged() -> GameEntities<2> {
    let mut e = GameEntities::EMPTY;
    e.spawn_from_records(&ENEMY);
    e.state[0] = GameEntityState::Aggro as u8;
    e.state_ticks[0] = 42;
    e
}
fn tick(
    e: &mut GameEntities<2>,
    m: &mut Arena,
    p: [i32; 3],
    delta: u16,
) -> GameEntityTickStats {
    e.tick_delta(&ENEMY, input(p), m, delta)
}

#[test]
fn stalk_run_hysteresis_and_real_walk_clip() {
    let mut e = engaged();
    e.combatants[0].stance.set_swap_cooldown(6000);
    let mut m = Arena::default();
    e.attack_cooldown[0] = 600;
    for _ in 0..8 {
        tick(&mut e, &mut m, [0, 0, 400], 2);
    }
    assert!(e.tactics[0].running);
    assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].run_clip);
    let z = e.pz(0);
    tick(&mut e, &mut m, [0, 0, z + 256], 2);
    assert!(e.tactics[0].running);
    let z = e.pz(0);
    tick(&mut e, &mut m, [0, 0, z + 180], 2);
    assert!(!e.tactics[0].running);
    assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].walk_clip);
    let z = e.pz(0);
    tick(&mut e, &mut m, [0, 0, z + 256], 2);
    assert!(!e.tactics[0].running);
}
#[test]
fn circle_keeps_side_until_it_may_replan_and_does_not_own_attack() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.begin_goal(0, EnemyGoal::CircleLeft, 240, 64);
    // A spacing circle is reconsidered from its 24th tick on (the energy
    // rules replan spacing); until then it keeps its side.
    for _ in 0..11 {
        tick(&mut e, &mut m, [0, 0, 100], 2);
        assert_eq!(e.tactics[0].goal, EnemyGoal::CircleLeft);
        assert_eq!(e.attack_owner(), None);
    }
    assert_eq!(e.tactics[0].remaining, 218);
    // Circling left around the target steps to the body's own right, and
    // the clip is the motor's: the step it took.
    assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].strafe_right_clip);
    tick(&mut e, &mut m, [0, 0, 100], 2);
    assert_ne!(e.tactics[0].goal, EnemyGoal::CircleLeft);
    assert_eq!(e.attack_owner(), None);
}
#[test]
fn turn_is_bounded_and_target_behind_cannot_be_attacked() {
    let mut e = engaged();
    let mut m = Arena::default();
    let before = e.position(0);
    tick(&mut e, &mut m, [0, 0, -30], 2);
    assert_eq!(e.state(0), GameEntityState::Aggro);
    assert_eq!(e.position(0), before);
    let angle = e.yaw_raw(0) as u16 & 4095;
    assert!(angle.min(4096 - angle) <= 68);
}
#[test]
fn post_attack_spacing_uses_current_distance_and_keeps_punish_window() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.state[0] = GameEntityState::Recover as u8;
    e.state_ticks[0] = 0;
    e.attack_owner_plus_one = 1;
    for _ in 0..11 {
        tick(&mut e, &mut m, [0, 0, 20], 2);
        assert_eq!(e.state(0), GameEntityState::Recover);
        assert_eq!(e.tactics[0].goal, EnemyGoal::None);
    }
    tick(&mut e, &mut m, [0, 0, 20], 2);
    assert_eq!(e.state(0), GameEntityState::Aggro);
    assert_ne!(e.tactics[0].goal, EnemyGoal::Approach);
    assert_eq!(e.attack_owner(), None);
}
#[test]
fn contextual_melee_varies_and_never_repeats_three_times() {
    let mut e = engaged();
    let mut kinds = [false; 2];
    let mut previous = 255;
    let mut repeats = 0;
    for _ in 0..200 {
        let k = e.choose_attack(&ENEMY[0], 0, input([0, 0, 30]), false);
        kinds[k as usize] = true;
        repeats = if k == previous { repeats + 1 } else { 1 };
        assert!(repeats <= 2);
        previous = k;
    }
    assert_eq!(kinds, [true, true]);
}
#[test]
fn blocked_movement_waits_retries_and_does_not_teleport() {
    let mut e = engaged();
    let mut m = Arena {
        blocked: true,
        ..Default::default()
    };
    e.attack_cooldown[0] = 6000;
    let start = e.position(0);
    let mut saw_wait = false;
    let mut saw_reposition = false;
    for _ in 0..500 {
        tick(&mut e, &mut m, [0, 0, 300], 2);
        saw_wait |= e.tactics[0].goal == EnemyGoal::WaitRetry;
        saw_reposition |= e.tactics[0].goal == EnemyGoal::Reposition;
        assert_eq!(e.position(0), start);
    }
    assert!(saw_wait && saw_reposition);
    assert_eq!(e.attack_owner(), None);
    assert!(
        m.steps < 1100,
        "bounded probes, including retry samples: {}",
        m.steps
    );
}
#[test]
fn useful_motion_after_wait_retries_without_claiming_arrival() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.begin_goal(0, EnemyGoal::WaitRetry, 120, 0);
    e.tactics[0].retries = 1;
    for _ in 0..30 {
        tick(&mut e, &mut m, [0, 0, 180], 2);
    }
    assert_eq!(e.tactics[0].goal, EnemyGoal::Approach);
    assert_eq!(e.tactics[0].result, EnemyGoalResult::Cancelled);
    assert!(e.pz(0) > 0);
}
#[test]
fn home_is_reached_physically_and_reacquisition_has_cooldown() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.set_axis(0, 0, 150);
    e.set_axis(0, 2, 100);
    let mut arrived = false;
    for _ in 0..500 {
        tick(&mut e, &mut m, [0, 0, 1000], 2);
        if e.state(0) == GameEntityState::Idle {
            arrived = true;
            break;
        }
    }
    assert!(
        arrived,
        "position {:?}, yaw {}, snapshot {:?}",
        e.position(0),
        e.yaw_raw(0),
        e.tactical_snapshot(0)
    );
    assert!(within_xz([e.px(0), e.pz(0)], [0, 0], 14));
    assert_eq!(e.tactics[0].result, EnemyGoalResult::Arrived);
    tick(&mut e, &mut m, [0, 0, 20], 2);
    assert_eq!(e.state(0), GameEntityState::Idle);
}
#[test]
fn hidden_target_is_not_tracked_and_eventually_returns_home() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.attack_cooldown[0] = 6000;
    tick(&mut e, &mut m, [0, 0, 140], 2);
    let known = e.tactics[0].last_seen;
    m.hidden = true;
    let mut returned = false;
    for _ in 0..180 {
        tick(&mut e, &mut m, [200, 0, 200], 2);
        assert_eq!(e.tactics[0].last_seen, known);
        returned |=
            e.tactics[0].goal == EnemyGoal::ReturnHome || e.state(0) == GameEntityState::Idle;
    }
    assert!(returned);
}
#[test]
fn stagger_cancels_goal_and_attack_ownership() {
    let mut e = engaged();
    e.begin_goal(0, EnemyGoal::CircleRight, 300, 64);
    e.attack_owner_plus_one = 1;
    e.apply_hit(&ENEMY, 0, VitalityChannelId::Two, 1, 100);
    assert_eq!(e.state(0), GameEntityState::Staggered);
    assert_eq!(e.tactics[0].goal, EnemyGoal::None);
    assert_eq!(e.tactics[0].result, EnemyGoalResult::Cancelled);
    assert_eq!(e.attack_owner(), None);
}
#[test]
fn goal_seconds_match_both_npc_cadences() {
    for delta in [1, 2] {
        let mut e = engaged();
        let mut m = Arena::default();
        // Hold is replanned at once under the energy rules, so the clock is
        // read off a circle before its replan point.
        e.begin_goal(0, EnemyGoal::CircleLeft, 240, 64);
        e.attack_cooldown[0] = 600;
        for _ in 0..(22 / delta) {
            tick(&mut e, &mut m, [0, 0, 100], delta);
        }
        assert_eq!(e.tactics[0].goal, EnemyGoal::CircleLeft);
        assert_eq!(e.tactics[0].remaining, 218);
    }
}
#[test]
fn single_enemy_encounter_replay_is_repeatable() {
    let run = || {
        let mut e = GameEntities::<2>::EMPTY;
        e.spawn_from_records(&ENEMY);
        let mut m = Arena::default();
        let mut rows = std::vec::Vec::new();
        let mut attacks = 0;
        for n in 0..3600 {
            let p = match n {
                0..300 => [0, 0, 350],
                300..900 => [0, 0, 90],
                900..1500 => [0, 0, 25],
                1500..2100 => [120, 0, 100],
                2100..2700 => [0, 0, 900],
                _ => [0, 0, 70],
            };
            let s = tick(&mut e, &mut m, p, 2);
            attacks += s.attack_enters;
            rows.push((
                e.position(0),
                e.yaw_raw(0),
                e.state[0],
                e.tactical_snapshot(0),
                e.selected_attack_kind(0),
            ));
        }
        assert!(attacks >= 4, "only {attacks} attacks");
        rows
    };
    let a = run();
    assert_eq!(a, run());
    if let Ok(path) = std::env::var("CORTEX_ENEMY_TRACE") {
        use std::fmt::Write;
        let mut s = std::string::String::from(
            "tick,x,y,z,yaw,state,goal,result,generation,remaining,running,retries,unseen,attack\n",
        );
        for (i, (p, y, state, t, k)) in a.iter().enumerate() {
            writeln!(
                s,
                "{},{},{},{},{},{},{:?},{:?},{},{},{},{},{},{}",
                i * 2,
                p[0],
                p[1],
                p[2],
                y,
                state,
                t.goal,
                t.result,
                t.generation,
                t.remaining,
                t.running,
                t.retries,
                t.unseen_ticks,
                k
            )
            .unwrap();
        }
        std::fs::write(path, s).unwrap();
    }
}

#[test]
fn locked_cannon_approach_keeps_a_firing_band_instead_of_shuffling() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.combatants[0].stance.set_active(VitalityChannelId::Two);
    e.combatants[0].stance.set_swap_cooldown(300);
    e.begin_goal(0, EnemyGoal::Approach, 300, 34);
    let mut fired = false;
    for _ in 0..100 {
        tick(&mut e, &mut m, [0, 0, 180], 2);
        assert_ne!(e.tactics[0].goal, EnemyGoal::Retreat);
        if e.state(0) == GameEntityState::Windup {
            assert_eq!(e.selected_attack_kind(0), GAME_ENTITY_ATTACK_RANGED);
            assert!(GameEntities::<2>::distance(e.position(0), [0, 0, 180]) > 60);
            fired = true;
            break;
        }
    }
    assert!(fired);
}

#[test]
fn sideways_motion_does_not_count_as_a_restored_approach() {
    struct Slide;
    impl StepRule for Slide {
        fn step(
            &mut self,
            _: usize,
            _: RoomIndex,
            p: [i32; 3],
            _: i32,
            _: i32,
            _: i32,
            _: i32,
        ) -> [i32; 3] {
            [p[0] + 2, p[1], p[2]]
        }
    }
    let mut e = engaged();
    e.begin_goal(0, EnemyGoal::WaitRetry, 120, 0);
    e.tactics[0].retries = 1;
    for _ in 0..30 {
        e.tick_delta(&ENEMY, input([0, 0, 180]), &mut Slide, 2);
    }
    assert_eq!(e.tactics[0].goal, EnemyGoal::WaitRetry);
    assert_eq!(e.tactics[0].retries, 1);
}

#[test]
fn tactical_state_has_a_fixed_small_memory_budget() {
    assert!(core::mem::size_of::<TacticalState>() <= 80);
    std::println!(
        "tactical bytes per actor: {}",
        core::mem::size_of::<TacticalState>()
    );
}

#[test]
fn hearing_acquires_a_location_without_tracking_hidden_player() {
    let mut e: GameEntities<2> = GameEntities::EMPTY;
    e.spawn_from_records(&ENEMY);
    let mut m = Arena {
        hidden: true,
        ..Default::default()
    };
    tick(&mut e, &mut m, [0, 0, 30], 2);
    assert_eq!(e.state(0), GameEntityState::Aggro);
    assert_eq!(e.tactics[0].last_seen, [0, 0, 30]);
    tick(&mut e, &mut m, [100, 0, 0], 2);
    assert_eq!(e.yaw_raw(0), 0);
    assert_eq!(e.tactics[0].last_seen, [0, 0, 30]);
    assert_ne!(e.state(0), GameEntityState::Windup);
}

#[test]
fn losing_sight_during_retreat_is_cancellation_not_arrival() {
    let mut e = engaged();
    e.remember_target(0, [0, 0, 30]);
    e.begin_goal(0, EnemyGoal::Retreat, 210, 64);
    let mut m = Arena {
        hidden: true,
        ..Default::default()
    };
    tick(&mut e, &mut m, [0, 0, 30], 2);
    assert_eq!(e.tactics[0].result, EnemyGoalResult::Cancelled);
}

#[test]
fn spacing_speed_slows_only_directional_movement_at_both_tick_rates() {
    for delta in [1, 2] {
        for goal in [
            EnemyGoal::CircleLeft,
            EnemyGoal::CircleRight,
            EnemyGoal::Retreat,
            EnemyGoal::Approach,
        ] {
            let mut displacements = [0; 2];
            for (index, percent) in [100, 50].into_iter().enumerate() {
                let mut record = enemy();
                record.spacing_speed_percent = percent;
                let records = std::boxed::Box::leak(std::boxed::Box::new([record]));
                let mut e = engaged();
                e.attack_cooldown[0] = 600;
                e.combatants[0].stance.set_swap_cooldown(600);
                let target = [0, 0, if goal == EnemyGoal::Retreat { 30 } else { 64 }];
                e.remember_target(0, target);
                e.begin_goal(
                    0,
                    goal,
                    210,
                    if goal == EnemyGoal::Approach { 12 } else { 64 },
                );
                // Twelve frames: the motor carries sub-unit steps between
                // ticks, so the share of the pace shows over several.
                let mut moved = false;
                for tick in 0..(12 / delta) {
                    e.tactics[0].requested = 0;
                    e.tactics[0].sample_ticks = 0;
                    e.tick_delta(records, input(target), &mut Arena::default(), delta);
                    if tick == 0 {
                        assert_eq!(e.tactics[0].goal, goal);
                        // The planned pace: the walk, at the spacing share
                        // and the locked-on strafe or backward pace.
                        let expected =
                            ((e.tactical_walk_speed_q8(&records[0], goal) * i32::from(delta)) >> 8)
                                .max(1);
                        assert_eq!(i32::from(e.tactics[0].requested), expected);
                    }
                    moved |= e.motor_anims[0] != CharacterMotorAnim::Idle;
                }
                assert!(moved);
                displacements[index] = e.px(0).abs() + e.pz(0).abs();
            }
            if goal == EnemyGoal::Approach {
                // Unaffected by the spacing share (whole units may differ by one).
                assert!(
                    (displacements[0] - displacements[1]).abs() <= 1,
                    "{displacements:?}"
                );
            } else {
                // Whole units of carried sub-unit steps may differ by one.
                assert!(
                    (displacements[0] - 2 * displacements[1]).abs() <= 2,
                    "{goal:?}, delta {delta}: {displacements:?}"
                );
            }
        }
    }
}

#[test]
fn post_attack_circling_uses_a_wider_personal_space() {
    let mut e = engaged();
    let mut saw_circle = false;
    for _ in 0..40 {
        e.choose_spacing(&ENEMY[0], 0, input([0, 0, 90]));
        if matches!(
            e.tactics[0].goal,
            EnemyGoal::CircleLeft | EnemyGoal::CircleRight
        ) {
            saw_circle = true;
            assert_eq!(e.tactics[0].separation, 96);
            // A short reset, not a multi-second refusal: 36 plus up to 36.
            assert!((36..=72).contains(&e.tactics[0].remaining));
        }
    }
    assert!(saw_circle);
}

#[test]
fn an_empty_cannon_leads_to_a_close_approach_after_spacing() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.combatants[0].stance.set_active(VitalityChannelId::Two);
    e.tactics[0].last_attack = GAME_ENTITY_ATTACK_RANGED;
    e.tactics[0].repeat_count = 2;
    e.attack_cooldown[0] = 240;
    // Energy decides the stance: a cannon with no shot left goes to the
    // blade to earn it back.
    e.combatants[0].flow.energy = 0;
    let mut approached = false;
    for _ in 0..100 {
        tick(&mut e, &mut m, [0, 0, 128], 2);
        approached |= e.stance(0) == VitalityChannelId::One && e.pz(0) >= 60;
    }
    assert!(approached, "hybrid stayed outside melee forever");
}

#[test]
fn brief_cover_during_retry_preserves_target_memory_for_reposition() {
    let mut e = engaged();
    e.remember_target(0, [0, 0, 100]);
    e.begin_goal(0, EnemyGoal::WaitRetry, 2, 0);
    e.tactics[0].retries = 1;
    let mut m = Arena {
        hidden: true,
        ..Default::default()
    };
    tick(&mut e, &mut m, [0, 0, 100], 2);
    assert_eq!(e.tactics[0].goal, EnemyGoal::Reposition);
}

#[test]
fn walking_back_to_the_same_wall_does_not_erase_retries() {
    let mut e = engaged();
    e.remember_target(0, [100, 0, 0]);
    e.begin_goal(0, EnemyGoal::Approach, 300, 30);
    e.tactics[0].retries = 2;
    e.tactics[0].recovery_distance = 64;
    e.tactics[0].sample_ticks = 58;
    e.tactics[0].start_distance = 80;
    e.tactics[0].travel = 100;
    e.tactics[0].requested = 100;
    e.set_axis(0, 0, 36); // Returned from the detour to the same obstructing wall.
    e.monitor_progress(&ENEMY[0], 0, [20, 0, 0], [100, 0, 0], 4, 2);
    assert_eq!(e.tactics[0].retries, 2);
}

#[test]
fn fractional_spacing_keeps_short_steps_and_cadence_without_idle_flicker() {
    for delta in [1, 2] {
        let mut r = enemy();
        r.spacing_speed_percent = 25;
        let records = std::boxed::Box::leak(std::boxed::Box::new([r]));
        let mut e = engaged();
        e.attack_cooldown[0] = 600;
        e.combatants[0].stance.set_swap_cooldown(600);
        e.remember_target(0, [0, 0, 30]);
        e.begin_goal(0, EnemyGoal::Retreat, 210, 128);
        let mut arena = Arena::default();
        for elapsed in (delta..=12).step_by(usize::from(delta)) {
            e.tick_delta(records, input([0, 0, 30]), &mut arena, delta);
            e.update_animation(records, SimTick::from_u32(u32::from(elapsed)), |_, _| 0);
            if elapsed >= 2 {
                assert_eq!(e.clip_for_state(records, 0).clip, r.walk_backward_clip);
            }
        }
        // Backing off while facing the target is the motor's locked-on
        // backward pace (40%), at a quarter of the walk: sub-unit steps the
        // motor carries, so it still gets somewhere.
        assert!(e.pz(0) < 0, "{}", e.pz(0));
        // The stride runs on the clock since the gait began (the first
        // think, or the second at one tick per think).
        let phase = e.clip_for_state(records, 0).phase_ticks;
        assert!((10..=12).contains(&phase), "{phase}");
        // A real collision stops the gait: the motor reports idle for a move
        // it refused (a sub-unit step tries no collision, so it walks on).
        arena.blocked = true;
        let mut stopped = false;
        for _ in 0..4 {
            e.tick_delta(records, input([0, 0, 30]), &mut arena, delta);
            stopped |= e.clip_for_state(records, 0).clip == r.idle_clip;
        }
        assert!(stopped);
    }
}

#[test]
fn terminal_presentation_advances_only_defeated_actors() {
    let mut entities = GameEntities::<2>::EMPTY;
    entities.spawn_from_records(&ENEMY);
    entities.set_spatial_active_mask(u64::MAX);
    let before = entities.state_ticks[0];
    entities.advance_defeated_animations(10);
    assert_eq!(entities.state_ticks[0], before);
    entities.state[0] = GameEntityState::Dead as u8;
    entities.advance_defeated_animations(10);
    assert_eq!(entities.state_ticks[0], before + 10);
    assert_eq!(entities.state(0), GameEntityState::Dead);
}

#[test]
fn elevated_target_does_not_bait_ground_melee() {
    let mut e = engaged();
    let mut arena = Arena::default();
    let mut fired = false;
    for _ in 0..900 {
        let mut target = input([0, 160, 180]);
        // Ordinarily this exposed-channel hint would request the melee stance.
        target.target.stance = Some(VitalityChannelId::Two);
        e.tick_delta(&ENEMY, target, &mut arena, 1);
        if e.state(0) == GameEntityState::Windup {
            assert_eq!(e.selected_attack_kind(0), GAME_ENTITY_ATTACK_RANGED);
            fired = true;
        }
    }
    assert!(
        fired,
        "hybrid must still pressure a visible overhead target"
    );
}

#[test]
fn energy_replan_interrupts_spacing_but_not_an_attack() {
    let mut e = engaged();
    e.mutate_stance(0);
    e.advance_stance_swap(0, 12);
    e.combatants[0].stance.set_swap_cooldown(0);
    for _ in 0..5 {
        e.spend_shot_energy(&ENEMY[0], 0);
    }
    e.begin_goal(0, EnemyGoal::CircleRight, 300, 64);
    e.tactics[0].phase = 24;
    tick(&mut e, &mut Arena::default(), [0, 0, 140], 2);
    assert_eq!(e.stance(0), VitalityChannelId::One);
    assert_eq!(e.tactics[0].stance_reason, 5);
    assert_ne!(e.tactics[0].goal, EnemyGoal::CircleRight);
    e.enter_state(
        0,
        GameEntityState::Attack,
        &mut GameEntityTickStats::default(),
    );
    e.combatants[0].stance.set_swap_cooldown(0);
    tick(&mut e, &mut Arena::default(), [0, 200, 140], 2);
    assert_eq!(e.stance(0), VitalityChannelId::One);
}

#[test]
fn empty_enemy_does_not_claw_at_an_overhead_perch_or_invent_energy() {
    let mut e = engaged();
    for _ in 0..5 {
        e.spend_shot_energy(&ENEMY[0], 0);
    }
    for _ in 0..150 {
        tick(&mut e, &mut Arena::default(), [0, 150, 100], 2);
    }
    assert_eq!(e.stance(0), VitalityChannelId::Two);
    assert_ne!(e.state(0), GameEntityState::Attack);
    assert_eq!(e.energy(0), 0);
    assert_eq!(e.attack_owner(), None);
}

#[test]
fn crowded_melee_tell_moves_through_collision_then_plants() {
    let mut e = engaged();
    e.select_attack(0, false);
    e.enter_state(
        0,
        GameEntityState::Windup,
        &mut GameEntityTickStats::default(),
    );
    let mut arena = Arena::default();
    e.step_melee_tell(&ENEMY[0], 0, input([0, 0, 26]), &mut arena, 2);
    assert!(e.position(0)[2] < 0);
    let planted = e.position(0);
    e.state_ticks[0] = 8;
    e.step_melee_tell(&ENEMY[0], 0, input([0, 0, 26]), &mut arena, 2);
    assert_eq!(e.position(0), planted);
    e.state_ticks[0] = 0;
    arena.blocked = true;
    e.step_melee_tell(&ENEMY[0], 0, input([0, 0, 26]), &mut arena, 2);
    assert_eq!(e.position(0), planted);
}

#[test]
fn heavy_contact_creates_firing_distance_before_attacking() {
    let mut e = engaged();
    e.combatants[0]
        .flow
        .melee_hit(true, &TEST_BODIES[usize::from(BODY_TACTICAL)].combat);
    let mut arena = Arena::default();
    let mut escaped = false;
    let mut attacked = false;
    for _ in 0..300 {
        tick(&mut e, &mut arena, [0, 0, 26], 2);
        escaped |= e.tactics[0].goal == EnemyGoal::BreakAway;
        if e.state(0) == GameEntityState::Windup {
            assert!(!e.target_within(0, input([0, 0, 26]), 192));
            assert!(e.selected_attack_is_ranged(0));
            attacked = true;
            break;
        }
    }
    assert!(
        escaped,
        "a connected heavy attack should create an escape opportunity"
    );
    assert!(attacked, "escape must lead back to a legal shot");
}

#[test]
fn blocked_breakaway_never_fakes_arrival_or_shoots_at_contact() {
    let mut e = engaged();
    let mut arena = Arena {
        blocked: true,
        ..Arena::default()
    };
    let start = e.position(0);
    let mut defended = false;
    for _ in 0..180 {
        tick(&mut e, &mut arena, [0, 0, 26], 2);
        assert_eq!(e.position(0), start);
        if matches!(
            e.state(0),
            GameEntityState::Windup | GameEntityState::Attack
        ) {
            assert!(!e.selected_attack_is_ranged(0));
            defended = true;
        }
    }
    assert!(defended, "blocked retreat should lead to a melee response");
}

#[test]
fn breakaway_defends_before_dragging_a_chaser_outside_the_home_leash() {
    let mut e = engaged();
    let edge = i32::from(ENEMY[0].aggro_radius);
    e.set_axis(0, 2, edge);
    let mut arena = Arena::default();
    let mut defended = false;
    for _ in 0..180 {
        tick(&mut e, &mut arena, [0, 0, edge - 26], 2);
        assert!(e.position(0)[2] < edge + 64);
        assert_ne!(e.tactics[0].goal, EnemyGoal::ReturnHome);
        if e.state(0) == GameEntityState::Windup {
            assert!(!e.selected_attack_is_ranged(0));
            defended = true;
            break;
        }
    }
    assert!(defended);
}

#[test]
fn boundary_defender_only_switches_to_cannon_when_there_is_firing_space() {
    let mut e = engaged();
    let edge = i32::from(ENEMY[0].aggro_radius);
    e.set_axis(0, 2, edge);
    e.attack_cooldown[0] = 600;
    let mut arena = Arena {
        blocked: true,
        ..Arena::default()
    };
    tick(&mut e, &mut arena, [0, 0, edge - 26], 2);
    assert_eq!(e.stance(0), VitalityChannelId::One);
    e.finish_goal(0, EnemyGoalResult::Cancelled);
    tick(&mut e, &mut arena, [0, 0, edge - 240], 2);
    assert_eq!(e.stance(0), VitalityChannelId::Two);
}

#[test]
fn bullet_evasion_uses_its_own_lane_and_respects_collision() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.tactics[0].last_seen = [0, 0, 224];
    e.move_yaw_valid[0] = 1;
    e.move_yaw[0] = 0;
    e.projectile_threats[0] = Some(crate::projectiles::ProjectileThreat {
        position: [0, 32, 160],
        velocity: [0, 0, -8],
        ticks_to_contact: 20,
    });
    let mut stats = GameEntityTickStats::default();
    let plan = e
        .plan_ranged_exchange(&ENEMY[0], 0, input([0, 0, 224]), &mut m, 1, 256, &mut stats)
        .expect("evading, not firing");
    e.apply_intent(&ENEMY[0], 0, plan.intent, &mut m, 1, &mut stats);
    assert!(e.px(0) > 0);
    assert_eq!(e.pz(0), 0);
    let before = e.position(0);
    m.blocked = true;
    let plan = e
        .plan_ranged_exchange(&ENEMY[0], 0, input([0, 0, 224]), &mut m, 1, 256, &mut stats)
        .expect("evading, not firing");
    e.apply_intent(&ENEMY[0], 0, plan.intent, &mut m, 1, &mut stats);
    assert_eq!(e.position(0), before);
}
#[test]
fn visible_melee_recovery_releases_an_active_defensive_retreat() {
    let mut e = engaged();
    let mut m = Arena::default();
    let mut stats = GameEntityTickStats::default();
    let mut swing = input([0, 0, 50]);
    swing.target.attack_read = 1;
    assert!(e
        .plan_melee_defense(&ENEMY[0], 0, swing, &mut m, 1, &mut stats)
        .is_some());
    assert_eq!(e.tactics[0].goal, EnemyGoal::Retreat);
    let mut recovery = input([0, 0, 50]);
    recovery.target.attack_read = 3;
    assert!(e
        .plan_melee_defense(&ENEMY[0], 0, recovery, &mut m, 1, &mut stats)
        .is_none());
    assert_eq!(e.tactics[0].goal, EnemyGoal::None);
    assert_eq!(e.combat_role(0), 7);
}

#[test]
fn empty_energy_melee_enemy_sidesteps_with_lateral_clip_then_plants() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.combatants[0].flow.energy = 0;
    e.combatants[0].stance.set_swap_cooldown(6000);
    e.projectile_threats[0] = Some(crate::projectiles::ProjectileThreat {
        position: [0, 32, 160],
        velocity: [0, 0, -8],
        ticks_to_contact: 20,
    });
    tick(&mut e, &mut m, [0, 0, 224], 2);
    assert_eq!(e.tactics[0].goal, EnemyGoal::Evade);
    assert_eq!(e.intent(0), GameEntityIntent::CircleRight);
    // The step aside is to the body's left (+x facing +z): the motor's clip.
    assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].strafe_left_clip);
    // Without the mobility kit the dodge is a walk aside through the motor:
    // the locked-on strafe pace (90% of the 2-unit walk) over two frames.
    assert_eq!(e.px(0), 3);
    assert_eq!(e.combatants[0].flow.energy, 0);
    e.projectile_threats[0] = None;
    let mut ticks = 0;
    while e.tactics[0].goal == EnemyGoal::Evade && ticks < 9 {
        tick(&mut e, &mut m, [0, 0, 224], 2);
        ticks += 1;
    }
    // The planned walk aside is the walk over the roll's 14 frames (28),
    // straight to the side, and then it ends.
    let p = e.position(0);
    assert!(p[0] > 8 && p[0] <= 28, "{p:?}");
    // At most the one tick after the dodge ended, closing in again.
    assert!(p[2].abs() <= 4, "{p:?}");
    assert_ne!(e.tactics[0].goal, EnemyGoal::Evade);
    assert_eq!(e.state(0), GameEntityState::Aggro);
}
#[test]
fn projectile_evade_does_not_cancel_committed_actions() {
    for state in [
        GameEntityState::Windup,
        GameEntityState::Attack,
        GameEntityState::Recover,
    ] {
        let mut e = engaged();
        let mut m = Arena::default();
        e.state[0] = state as u8;
        e.state_ticks[0] = 0;
        e.projectile_threats[0] = Some(crate::projectiles::ProjectileThreat {
            position: [0, 32, 160],
            velocity: [0, 0, -8],
            ticks_to_contact: 20,
        });
        tick(&mut e, &mut m, [0, 0, 224], 2);
        assert_ne!(e.tactics[0].goal, EnemyGoal::Evade);
        assert_eq!(e.state(0), state);
    }
}

/// The test enemy with `flags` in place of its own.
fn enemy_with_flags(flags: u16) -> &'static [LevelGameEntityRecord; 1] {
    std::boxed::Box::leak(std::boxed::Box::new([LevelGameEntityRecord {
        flags,
        ..enemy()
    }]))
}

/// What entity 0 commits to when its controller asks for `attack`.
fn committed(records: &'static [LevelGameEntityRecord; 1], attack: CombatAction) -> Option<u8> {
    let mut e: GameEntities<2> = GameEntities::EMPTY;
    e.spawn_from_records(records);
    e.state[0] = GameEntityState::Aggro as u8;
    let mut stats = GameEntityTickStats::default();
    let intent = CombatIntent {
        pressed: crate::combat_intent::CombatActions::of(attack),
        aim: Some([0, 0, 100]),
        ..CombatIntent::IDLE
    };
    e.apply_intent(&records[0], 0, intent, &mut Arena::default(), 2, &mut stats);
    (e.state(0) == GameEntityState::Windup).then(|| e.selected_attack_kind(0))
}

#[test]
fn enemy_attacks_pass_the_same_kit_gate_as_the_player() {
    use game_entity_flags::{
        ENABLED, HEAVY_ATTACK, LIGHT_ATTACK, MELEE_ATTACK, RANGED_ATTACK,
    };
    let base = ENABLED;
    let both_strikes = enemy_with_flags(base | MELEE_ATTACK | LIGHT_ATTACK | HEAVY_ATTACK);
    assert_eq!(committed(both_strikes, CombatAction::LightAttack), Some(0));
    assert_eq!(committed(both_strikes, CombatAction::HeavyAttack), Some(1));
    // A heavy with no heavy clip is the light strike.
    let light_only = enemy_with_flags(base | MELEE_ATTACK | LIGHT_ATTACK);
    assert_eq!(committed(light_only, CombatAction::HeavyAttack), Some(0));
    // No blade, no strike at all; no cannon, no shot.
    let gunner = enemy_with_flags(base | RANGED_ATTACK);
    assert_eq!(committed(gunner, CombatAction::LightAttack), None);
    assert_eq!(committed(gunner, CombatAction::HeavyAttack), None);
    assert_eq!(committed(gunner, CombatAction::Shoot), Some(2));
    assert_eq!(committed(both_strikes, CombatAction::Shoot), None);
    // The player's gate gives the same answers for the same bindings.
    let kits = psx_level::game_entity_combat_kits(light_only[0].flags);
    assert_eq!(
        psx_level::combat_verbs::attack_for(
            kits,
            &psx_level::GameEntityBindings(light_only[0].flags),
            psx_level::CharacterAnimationAction::HeavyAttack,
        ),
        Some(psx_level::CharacterAnimationAction::LightAttack)
    );
}

#[test]
fn an_enemy_swaps_on_purpose_to_parry_a_swing_it_sees() {
    let records = enemy_with_flags(enemy().flags);
    let mut e: GameEntities<2> = GameEntities::EMPTY;
    e.spawn_from_records(&records[..]);
    e.set_swap_cooldown_for_tests(300);
    e.state[0] = GameEntityState::Aggro as u8;
    e.state_ticks[0] = 42;
    let stance = e.stance(0);
    let mut swing = input([0, 0, 40]);
    swing.target.attack_read = 2;
    swing.target.swing_ticks = Some(0);
    // Never on purpose at zero percent.
    let never: &'static [LevelGameEntityRecord] = &records[..];
    let mut quiet: GameEntities<2> = GameEntities::EMPTY;
    quiet.set_characters(tactical_bodies(|c| c.parry_swap_percent = 0));
    quiet.spawn_from_records(never);
    quiet.state[0] = GameEntityState::Aggro as u8;
    quiet.state_ticks[0] = 42;
    quiet.tick_delta(never, swing, &mut Arena::default(), 2);
    assert_eq!(quiet.stance(0), stance);
    // Always at a hundred: the swing it sees lands in its parry window.
    let always: &'static [LevelGameEntityRecord] = &records[..];
    e.set_characters(tactical_bodies(|c| c.parry_swap_percent = 100));
    e.tick_delta(always, swing, &mut Arena::default(), 2);
    assert_ne!(e.stance(0), stance);
    assert_eq!(e.tactical_snapshot(0).stance_reason, STANCE_REASON_PARRY);
    assert!(e.combatants[0].parrying(e.body(&always[0]).combat.parry_ticks));
}

#[test]
fn a_predicted_contact_replaces_the_swings_own_clock_for_the_parry() {
    let always: &'static [LevelGameEntityRecord] = &enemy_with_flags(enemy().flags)[..];
    let bodies = tactical_bodies(|c| c.parry_swap_percent = 100);
    let fresh = || {
        let mut e: GameEntities<2> = GameEntities::EMPTY;
        e.set_characters(bodies);
        e.spawn_from_records(always);
        e.set_swap_cooldown_for_tests(300);
        e.state[0] = GameEntityState::Aggro as u8;
        e.state_ticks[0] = 42;
        e
    };
    // The swing's clock says contact now; the posed blade does not reach
    // within the window, so no swap.
    let mut swing = input([0, 0, 40]);
    swing.target.attack_read = 2;
    swing.target.swing_ticks = Some(0);
    let mut e = fresh();
    let stance = e.stance(0);
    e.predict_swing_contact(0, PredictedContact::Outside);
    e.tick_delta(always, swing, &mut Arena::default(), 2);
    assert_eq!(e.stance(0), stance, "no real contact coming: no parry swap");
    // The clock says nothing is coming, the blade meets it next tick: swap.
    swing.target.swing_ticks = None;
    let mut e = fresh();
    e.predict_swing_contact(0, PredictedContact::In(1));
    e.tick_delta(always, swing, &mut Arena::default(), 2);
    assert_ne!(e.stance(0), stance);
    assert_eq!(e.tactical_snapshot(0).stance_reason, STANCE_REASON_PARRY);
}

#[test]
fn a_deliberate_parry_needs_the_swing_seen_its_reaction_time_before_contact() {
    let slow: &'static [LevelGameEntityRecord] = &enemy_with_flags(enemy().flags)[..];
    let bodies = tactical_bodies(|c| {
        c.parry_swap_percent = 100;
        c.parry_ticks = 3;
        c.parry_reaction_ticks = 12;
    });
    let fresh = || {
        let mut e: GameEntities<2> = GameEntities::EMPTY;
        e.set_characters(bodies);
        e.spawn_from_records(slow);
        e.set_swap_cooldown_for_tests(300);
        e.state[0] = GameEntityState::Aggro as u8;
        e.state_ticks[0] = 42;
        e
    };
    let mut swing = input([0, 0, 40]);
    swing.target.attack_read = 2;
    // First seen already inside the window: a perfect read, no swap.
    swing.target.swing_ticks = Some(1);
    let mut e = fresh();
    let stance = e.stance(0);
    e.tick_delta(slow, swing, &mut Arena::default(), 2);
    assert_eq!(e.stance(0), stance, "seen 1 tick before contact: no parry");
    // Seen from its tell, 13 ticks out, then followed in: the swap comes at
    // the window's entry.
    let mut e = fresh();
    swing.target.attack_read = 1;
    for ticks in [13, 11, 9, 7, 5, 3, 1] {
        swing.target.swing_ticks = Some(ticks);
        e.tick_delta(slow, swing, &mut Arena::default(), 2);
    }
    assert_ne!(e.stance(0), stance);
    assert_eq!(e.tactical_snapshot(0).stance_reason, STANCE_REASON_PARRY);
}

/// Put entity 0 in `stance`, its swap long finished and its cooldown clear
/// (as a parry leaves it).
fn settle_in(e: &mut GameEntities<2>, stance: VitalityChannelId) {
    if e.stance(0) != stance {
        e.combatants[0].stance.set_swap_cooldown(0);
        e.mutate_stance(0);
    }
    e.advance_stance_swap(0, 100);
    e.combatants[0].stance.set_swap_cooldown(0);
}

#[test]
fn a_parry_crit_is_spent_with_the_swapped_to_weapon() {
    let records = enemy_with_flags(
        enemy().flags | game_entity_flags::LIGHT_ATTACK | game_entity_flags::HEAVY_ATTACK,
    );
    // Blade stance, crit armed, target in reach: the crit strike.
    let mut e: GameEntities<2> = GameEntities::EMPTY;
    e.spawn_from_records(&records[..]);
    e.state[0] = GameEntityState::Aggro as u8;
    e.state_ticks[0] = 42;
    settle_in(&mut e, VitalityChannelId::One);
    e.attack_cooldown[0] = 600;
    e.combatants[0].crit = TEST_BODIES[usize::from(BODY_TACTICAL)]
        .combat
        .crit_window_ticks;
    assert_eq!(e.attack_owner(), None);
    e.tick_delta(&records[..], input([0, 0, 30]), &mut Arena::default(), 2);
    assert_eq!(e.state(0), GameEntityState::Windup);
    assert_eq!(e.selected_attack_kind(0), GAME_ENTITY_ATTACK_LIGHT);
    assert_eq!(e.attack_owner(), Some(0));
    // The strike reaches its active frames inside the crit window: it is
    // the crit strike (double damage and poise under the placeholders).
    let mut ticks = 0;
    while e.state(0) == GameEntityState::Windup {
        e.tick_delta(&records[..], input([0, 0, 30]), &mut Arena::default(), 2);
        ticks += 2;
    }
    assert_eq!(e.state(0), GameEntityState::Attack);
    assert!(
        ticks
            < TEST_BODIES[usize::from(BODY_TACTICAL)]
                .combat
                .crit_window_ticks
    );
    assert!(e.crit_armed(0));
    assert_eq!(e.crit_scaled(&records[..], 0, 10, 3), (20, 6));
    // Zenith stance, crit armed, energy to spare: the crit shot, even at range.
    let mut g: GameEntities<2> = GameEntities::EMPTY;
    g.spawn_from_records(&records[..]);
    g.state[0] = GameEntityState::Aggro as u8;
    g.state_ticks[0] = 42;
    settle_in(&mut g, VitalityChannelId::Two);
    g.attack_cooldown[0] = 600;
    g.combatants[0].crit = TEST_BODIES[usize::from(BODY_TACTICAL)]
        .combat
        .crit_window_ticks;
    g.tick_delta(&records[..], input([0, 0, 200]), &mut Arena::default(), 2);
    assert_eq!(g.state(0), GameEntityState::Windup);
    assert_eq!(g.selected_attack_kind(0), GAME_ENTITY_ATTACK_RANGED);
    // No crit: the same enemy on cooldown does not attack.
    let mut h: GameEntities<2> = GameEntities::EMPTY;
    h.spawn_from_records(&records[..]);
    h.state[0] = GameEntityState::Aggro as u8;
    h.state_ticks[0] = 42;
    h.attack_cooldown[0] = 600;
    h.tick_delta(&records[..], input([0, 0, 30]), &mut Arena::default(), 2);
    assert_ne!(h.state(0), GameEntityState::Windup);
}

#[test]
fn a_committed_swing_reads_its_record_contact_ticks() {
    let records = enemy_with_flags(enemy().flags);
    let mut e: GameEntities<2> = GameEntities::EMPTY;
    e.spawn_from_records(&records[..]);
    e.state[0] = GameEntityState::Aggro as u8;
    assert_eq!(e.committed_swing(&records[..], 0), None);
    let mut stats = GameEntityTickStats::default();
    let intent = CombatIntent {
        pressed: crate::combat_intent::CombatActions::of(CombatAction::LightAttack),
        ..CombatIntent::IDLE
    };
    e.apply_intent(&records[0], 0, intent, &mut Arena::default(), 2, &mut stats);
    let (action, first, last) = e.committed_swing(&records[..], 0).expect("winding up");
    assert_eq!(action, psx_level::CharacterAnimationAction::LightAttack);
    assert_eq!(first, u16::from(records[0].windup_ticks));
    assert_eq!(last, first + records[0].attack_active_ticks - 1);
    // Five ticks in, the record-timed swing is eleven ticks from contact.
    let clip = e.clip_for_state(&records[..], 0);
    assert_eq!(
        crate::combat::ticks_to_contact(first, last, u32::from(clip.phase_ticks + 5) << 12, 4096),
        Some(first - clip.phase_ticks - 5)
    );
}

/// An owner that drives entity 0 itself through the possession seam
/// ([`FighterWorld::controller_intent`]), over open ground.
struct Possessed(CombatIntent);

impl FighterWorld for Possessed {
    fn move_body(
        &mut self,
        entity: usize,
        room: RoomIndex,
        motor: &mut CharacterMotorState,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta: u16,
    ) -> CharacterMotorFrame {
        crate::entities::OpenWorld.move_body(entity, room, motor, input, config, delta)
    }

    fn push_body(
        &mut self,
        entity: usize,
        room: RoomIndex,
        position: [i32; 3],
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
    ) -> [i32; 3] {
        crate::entities::OpenWorld.push_body(entity, room, position, dx, dz, radius, height)
    }

    fn controller_intent(&mut self, _entity: usize) -> Option<CombatIntent> {
        Some(self.0)
    }
}

#[test]
fn a_possessed_entity_runs_its_owners_intent_instead_of_its_tactics() {
    use crate::combat_intent::{CombatAction, CombatActions, CombatMove};
    let target = [0, 0, 400];
    // Left to its tactics, the entity closes on the target.
    let mut free = engaged();
    free.combatants[0].stance.set_swap_cooldown(6000);
    free.attack_cooldown[0] = 600;
    for _ in 0..20 {
        tick(&mut free, &mut Arena::default(), target, 2);
    }
    assert!(free.pz(0) > 0, "the tactics approach");
    // Possessed by a pad that asks nothing, it stays where it is.
    let mut held = engaged();
    held.combatants[0].stance.set_swap_cooldown(6000);
    held.attack_cooldown[0] = 600;
    let start = held.position(0);
    let mut idle = Possessed(CombatIntent::IDLE);
    for _ in 0..20 {
        held.tick_delta(&ENEMY, input(target), &mut idle, 2);
    }
    assert_eq!(held.position(0), start, "an idle pad holds it");
    // A pad pushing along -x moves it that way, away from the target.
    let mut away = Possessed(CombatIntent {
        movement: CombatMove::Move {
            x: -4096,
            z: 0,
            forward: 0,
        },
        ..CombatIntent::IDLE
    });
    for _ in 0..20 {
        held.tick_delta(&ENEMY, input(target), &mut away, 2);
    }
    assert!(
        held.px(0) < start[0],
        "the pad's direction, not the tactics'"
    );
    assert!(held.pz(0) <= start[2]);
    // A pad's strike starts the same windup the tactics would.
    let mut strike = Possessed(CombatIntent {
        pressed: CombatActions::of(CombatAction::LightAttack),
        ..CombatIntent::IDLE
    });
    held.tick_delta(&ENEMY, input(target), &mut strike, 2);
    assert_eq!(held.state(0), GameEntityState::Windup);
}

#[test]
fn the_tactical_controller_decides_for_the_actor_as_for_an_entity() {
    let mut e = engaged();
    let actor = GameEntities::<2>::ACTOR_SLOT;
    // Her body far from the entity and facing it, the entity's own persona
    // at her spot.
    e.actor_motor_mut().snap_to(
        psx_engine::RoomPoint::new(0, 0, 600),
        psx_engine::Angle::HALF,
    );
    let mut persona = enemy();
    persona.z = 600;
    e.engage_actor(0, 7);
    assert_eq!(e.rival(actor), Some(0));
    // The controller reads her body where it is.
    assert_eq!(e.position(actor), [0, 0, 600]);
    let mut world = crate::entities::OpenWorld;
    // In sight: it acts on its rival as an entity does (closes in, or
    // swaps to shoot). Nothing moves her here, so it keeps asking.
    let acts = (0..30)
        .map(|_| e.actor_intent(&persona, &ENEMY, &mut world, 2, 0))
        .filter(|intent| {
            matches!(intent.movement, CombatMove::Move { .. }) || !intent.pressed.is_empty()
        })
        .count();
    assert!(acts > 0);
    // Held by an action, it does not decide; when an attack ends, the
    // persona's cooldown starts as an entity's recovery starts it.
    assert_eq!(
        e.actor_intent(&persona, &ENEMY, &mut world, 2, 2),
        CombatIntent::IDLE
    );
    e.actor_intent(&persona, &ENEMY, &mut world, 2, 0);
    assert!(e.attack_cooldown[actor] > 0);
}

/// The cooked training enemy's walk (512 Q8) at the game's 3/4 speed knob:
/// every pace below lands a whole unit only every few thinks.
fn sub_unit_enemy() -> &'static [LevelGameEntityRecord; 1] {
    let mut r = enemy();
    r.character = psx_level::CharacterIndex(BODY_SUB_UNIT);
    r.spacing_speed_percent = 25;
    std::boxed::Box::leak(std::boxed::Box::new([r]))
}

/// One 30 Hz think and the presentation tick after it, at `now`.
fn think_and_show(
    e: &mut GameEntities<2>,
    records: &'static [LevelGameEntityRecord],
    target: [i32; 3],
    now: &mut u32,
) -> GameEntityAnimation {
    e.tick_delta(records, input(target), &mut Arena::default(), 2);
    *now += 2;
    e.update_animation(records, SimTick::from_u32(*now), |_, _| 40);
    e.animation(0)
}

#[test]
fn a_sub_unit_circle_keeps_one_gait_on_a_continuous_clock() {
    let records = sub_unit_enemy();
    let mut e: GameEntities<2> = GameEntities::EMPTY;
    e.spawn_from_records(&records[..]);
    e.state[0] = GameEntityState::Aggro as u8;
    e.state_ticks[0] = 42;
    e.attack_cooldown[0] = 600;
    e.combatants[0].stance.set_swap_cooldown(600);
    e.remember_target(0, [0, 0, 100]);
    e.begin_goal(0, EnemyGoal::CircleLeft, 240, 64);
    let mut now = 100;
    let first = think_and_show(&mut e, &records[..], [0, 0, 100], &mut now);
    assert_eq!(first.clip.clip, records[0].strafe_right_clip);
    let mut phase = first.clip.phase_ticks;
    let (mut whole_steps, mut still_thinks) = (0, 0);
    for _ in 0..10 {
        let before = e.position(0);
        let shown = think_and_show(&mut e, &records[..], [0, 0, 100], &mut now);
        if e.position(0) == before {
            still_thinks += 1;
        } else {
            whole_steps += 1;
        }
        // No flapping to idle on a think with no whole unit, and the stride
        // advances by the think's two ticks every think.
        assert_eq!(shown.clip.clip, records[0].strafe_right_clip);
        assert_eq!(shown.clip.phase_ticks, phase + 2);
        phase = shown.clip.phase_ticks;
    }
    assert!(
        still_thinks > 0 && whole_steps > 0,
        "{still_thinks} {whole_steps}"
    );
}

#[test]
fn a_ranged_windup_backing_off_keeps_its_legs_on_the_gait() {
    let records = sub_unit_enemy();
    let mut e: GameEntities<2> = GameEntities::EMPTY;
    e.spawn_from_records(&records[..]);
    e.state[0] = GameEntityState::Aggro as u8;
    e.state_ticks[0] = 42;
    settle_in(&mut e, VitalityChannelId::Two);
    e.attack_cooldown[0] = 0;
    let mut now = 100;
    for _ in 0..300 {
        think_and_show(&mut e, &records[..], [0, 0, 200], &mut now);
        if e.state(0) == GameEntityState::Windup {
            break;
        }
    }
    assert_eq!(e.state(0), GameEntityState::Windup);
    assert!(e.selected_attack_is_ranged(0));
    // The target steps inside the band: the shot is wound up backing off.
    let p = e.position(0);
    let target = [p[0], p[1], p[2] + 40];
    let mut legs_phase = None;
    let (mut whole_steps, mut still_thinks) = (0, 0);
    while e.state(0) == GameEntityState::Windup {
        let before = e.position(0);
        let shown = think_and_show(&mut e, &records[..], target, &mut now);
        if e.position(0) == before {
            still_thinks += 1;
        } else {
            whole_steps += 1;
        }
        if e.state(0) != GameEntityState::Windup {
            break;
        }
        let (legs, _) = shown.legs.expect("the legs stay on the gait every think");
        assert_eq!(legs.clip, records[0].walk_backward_clip);
        if let Some(previous) = legs_phase {
            assert_eq!(legs.phase_ticks, previous + 2);
        }
        legs_phase = Some(legs.phase_ticks);
    }
    assert!(
        still_thinks > 0 && whole_steps > 0,
        "{still_thinks} {whole_steps}"
    );
}

#[test]
fn a_clip_change_crossfades_and_carries_the_stride() {
    let records = sub_unit_enemy();
    let mut e: GameEntities<2> = GameEntities::EMPTY;
    e.spawn_from_records(&records[..]);
    e.state[0] = GameEntityState::Aggro as u8;
    e.state_ticks[0] = 42;
    e.attack_cooldown[0] = 600;
    e.combatants[0].stance.set_swap_cooldown(600);
    e.remember_target(0, [0, 0, 100]);
    e.begin_goal(0, EnemyGoal::CircleLeft, 240, 64);
    let mut now = 100;
    for _ in 0..8 {
        think_and_show(&mut e, &records[..], [0, 0, 100], &mut now);
    }
    let strafing = e.animation(0).clip;
    assert_eq!(strafing.clip, records[0].strafe_right_clip);
    // The motor now reports a backward walk: gait to gait, the stride keeps
    // its place in the cycle (both clips 40 ticks here) and the strafe fades.
    e.motor_anims[0] = CharacterMotorAnim::WalkBackward;
    now += 2;
    e.update_animation(&records[..], SimTick::from_u32(now), |_, _| 40);
    let shown = e.animation(0);
    assert_eq!(shown.clip.clip, records[0].walk_backward_clip);
    assert_eq!(shown.clip.phase_ticks % 40, (strafing.phase_ticks + 2) % 40);
    let (from, weight) = shown.fade.expect("a gait change crossfades");
    assert_eq!(from.clip, records[0].strafe_right_clip);
    assert_eq!(weight, 0);
    // Stopping: the walk fades into idle over the stop window, its pose held.
    e.motor_anims[0] = CharacterMotorAnim::Idle;
    now += 2;
    e.update_animation(&records[..], SimTick::from_u32(now), |_, _| 40);
    now += 2;
    e.update_animation(&records[..], SimTick::from_u32(now), |_, _| 40);
    let shown = e.animation(0);
    assert_eq!(shown.clip.clip, records[0].idle_clip);
    let (from, weight) = shown.fade.expect("a stop crossfades");
    assert_eq!(from.clip, records[0].walk_backward_clip);
    assert_eq!(weight, 2048);
}
