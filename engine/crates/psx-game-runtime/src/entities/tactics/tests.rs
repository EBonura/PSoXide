extern crate std;
use super::*;
use crate::entities::tests::test_record;

const fn enemy() -> LevelGameEntityRecord {
    let mut r = test_record(
        0,
        0,
        0,
        400,
        game_entity_flags::ENABLED
            | game_entity_flags::CAN_RUN
            | game_entity_flags::TACTICAL
            | game_entity_flags::RANGED_ATTACK,
    );
    r.radius = 14;
    r.height = 77;
    r.walk_speed = 2;
    r.run_speed = 6;
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
    r.max_health = 600;
    r.max_health_secondary = 600;
    r
}
static ENEMY: [LevelGameEntityRecord; 1] = [enemy()];
static ROOMS: [RoomIndex; 1] = [RoomIndex(0)];
fn input(p: [i32; 3]) -> GameEntityTickInput<'static> {
    GameEntityTickInput {
        player: p,
        player_room: RoomIndex(0),
        player_radius: 12,
        player_height: 64,
        player_noise_radius: 100,
        player_invulnerable: false,
        player_combat: None,
        active_rooms: &ROOMS,
    }
}
#[derive(Default)]
struct Arena {
    blocked: bool,
    hidden: bool,
    wall: bool,
    steps: u32,
}
impl GameEntityMover for Arena {
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
fn engaged() -> GameEntities<2, true> {
    let mut e = GameEntities::EMPTY;
    e.spawn_from_records(&ENEMY);
    e.state[0] = GameEntityState::Aggro as u8;
    e.state_ticks[0] = 42;
    e
}
fn tick(
    e: &mut GameEntities<2, true>,
    m: &mut Arena,
    p: [i32; 3],
    delta: u16,
) -> GameEntityTickStats {
    e.tick_delta(&ENEMY, input(p), m, delta)
}

#[test]
fn stalk_run_hysteresis_and_real_walk_clip() {
    let mut e = engaged();
    e.stance_swap_cooldown[0] = 6000;
    let mut m = Arena::default();
    e.attack_cooldown[0] = 600;
    for _ in 0..8 {
        tick(&mut e, &mut m, [0, 0, 400], 2);
    }
    assert!(e.tactics[0].running);
    assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].run_clip);
    let z = e.z[0];
    tick(&mut e, &mut m, [0, 0, z + 256], 2);
    assert!(e.tactics[0].running);
    let z = e.z[0];
    tick(&mut e, &mut m, [0, 0, z + 180], 2);
    assert!(!e.tactics[0].running);
    assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].walk_clip);
    let z = e.z[0];
    tick(&mut e, &mut m, [0, 0, z + 256], 2);
    assert!(!e.tactics[0].running);
}
#[test]
fn circle_keeps_side_for_seconds_and_does_not_own_attack() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.begin_goal(0, EnemyGoal::CircleLeft, 240, 64);
    for _ in 0..100 {
        tick(&mut e, &mut m, [0, 0, 100], 2);
        assert_eq!(e.tactics[0].goal, EnemyGoal::CircleLeft);
        assert_eq!(e.attack_owner(), None);
    }
    assert_eq!(e.tactics[0].remaining, 40);
    assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].strafe_left_clip);
}
#[test]
fn turn_is_bounded_and_target_behind_cannot_be_attacked() {
    let mut e = engaged();
    let mut m = Arena::default();
    let before = e.position(0);
    tick(&mut e, &mut m, [0, 0, -30], 2);
    assert_eq!(e.state(0), GameEntityState::Aggro);
    assert_eq!(e.position(0), before);
    let angle = e.yaw[0] as u16 & 4095;
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
        e.choose_attack(&ENEMY[0], 0, input([0, 0, 30]), false);
        let k = e.selected_attack_kind(0);
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
    assert!(e.z[0] > 0);
}
#[test]
fn home_is_reached_physically_and_reacquisition_has_cooldown() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.x[0] = 150;
    e.z[0] = 100;
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
        e.yaw[0],
        e.tactical_snapshot(0)
    );
    assert!(within_xz([e.x[0], e.z[0]], [0, 0], 14));
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
        e.begin_goal(0, EnemyGoal::Hold, 60, 64);
        e.attack_cooldown[0] = 600;
        for _ in 0..(58 / delta) {
            tick(&mut e, &mut m, [0, 0, 100], delta);
        }
        assert_eq!(e.tactics[0].remaining, 2);
    }
}
#[test]
fn single_enemy_encounter_replay_is_repeatable() {
    let run = || {
        let mut e = GameEntities::<2, true>::EMPTY;
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
                e.yaw[0],
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
    e.combat_flags[0] |= GAME_ENTITY_STANCE_ZENITH;
    e.stance_swap_cooldown[0] = 300;
    e.begin_goal(0, EnemyGoal::Approach, 300, 34);
    let mut fired = false;
    for _ in 0..100 {
        tick(&mut e, &mut m, [0, 0, 180], 2);
        assert_ne!(e.tactics[0].goal, EnemyGoal::Retreat);
        if e.state(0) == GameEntityState::Windup {
            assert_eq!(e.selected_attack_kind(0), GAME_ENTITY_ATTACK_RANGED);
            assert!(GameEntities::<2, true>::distance(e.position(0), [0, 0, 180]) > 60);
            fired = true;
            break;
        }
    }
    assert!(fired);
}

#[test]
fn sideways_motion_does_not_count_as_a_restored_approach() {
    struct Slide;
    impl GameEntityMover for Slide {
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
    let mut e: GameEntities<2, true> = GameEntities::EMPTY;
    e.spawn_from_records(&ENEMY);
    let mut m = Arena {
        hidden: true,
        ..Default::default()
    };
    tick(&mut e, &mut m, [0, 0, 30], 2);
    assert_eq!(e.state(0), GameEntityState::Aggro);
    assert_eq!(e.tactics[0].last_seen, [0, 0, 30]);
    tick(&mut e, &mut m, [100, 0, 0], 2);
    assert_eq!(e.yaw[0], 0);
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
                e.stance_swap_cooldown[0] = 600;
                let target = [0, 0, if goal == EnemyGoal::Retreat { 30 } else { 64 }];
                e.remember_target(0, target);
                e.begin_goal(
                    0,
                    goal,
                    210,
                    if goal == EnemyGoal::Approach { 12 } else { 64 },
                );
                e.tick_delta(records, input(target), &mut Arena::default(), delta);
                assert_eq!(e.tactics[0].goal, goal);
                assert!(e.tactics[0].moved);
                displacements[index] = e.x[0].abs() + e.z[0].abs();
                let expected_speed = if goal == EnemyGoal::Approach || percent == 100 {
                    2
                } else {
                    1
                };
                assert_eq!(e.tactics[0].requested, expected_speed * delta);
            }
            if goal == EnemyGoal::Approach {
                assert_eq!(displacements[0], displacements[1]);
            } else {
                // Integer Q12 steering may differ by one world unit.
                assert!(
                    (displacements[0] - 2 * displacements[1]).abs() <= 1,
                    "{goal:?}, delta {delta}"
                );
            }
        }
    }
}

#[test]
fn replanning_same_gait_keeps_its_animation_phase() {
    let mut e = engaged();
    e.begin_goal(0, EnemyGoal::Approach, 300, 30);
    e.z[0] = 4;
    e.advance_tactical_animation(&ENEMY[0], 0, [0, 0, 0], 2);
    assert_eq!(e.clip_for_state(&ENEMY, 0).phase_ticks, 2);
    e.begin_goal(0, EnemyGoal::Approach, 300, 30);
    e.z[0] = 8;
    e.advance_tactical_animation(&ENEMY[0], 0, [0, 0, 4], 2);
    assert_eq!(e.clip_for_state(&ENEMY, 0).phase_ticks, 4);
}

#[test]
fn gait_clock_tracks_committed_travel_at_both_cadences() {
    for delta in [1, 2] {
        let mut e = engaged();
        e.begin_goal(0, EnemyGoal::CircleLeft, 240, 96);
        for _ in 0..(12 / delta) {
            let before = e.position(0);
            e.x[0] -= i32::from(delta); // half of the authored 2-unit walk speed
            e.advance_tactical_animation(&ENEMY[0], 0, before, delta);
        }
        assert_eq!(e.clip_for_state(&ENEMY, 0).phase_ticks, 6);
        let before = e.position(0);
        e.advance_tactical_animation(&ENEMY[0], 0, before, delta);
        assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].idle_clip);
    }
}

#[test]
fn diagonal_gait_uses_distance_instead_of_axis_maximum() {
    let mut e = engaged();
    e.begin_goal(0, EnemyGoal::Approach, 300, 30);
    for _ in 0..8 {
        let before = e.position(0);
        e.x[0] += 3;
        e.z[0] += 3;
        e.advance_tactical_animation(&ENEMY[0], 0, before, 2);
    }
    // sqrt(18) * 8 / authored speed 2, rounded to ticks.
    assert_eq!(e.clip_for_state(&ENEMY, 0).phase_ticks, 16);
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
            assert!((210..=300).contains(&e.tactics[0].remaining));
        }
    }
    assert!(saw_circle);
}

#[test]
fn slower_spacing_gait_clock_uses_its_own_nominal_speed() {
    let mut r = enemy();
    r.spacing_speed_percent = 50;
    for goal in [
        EnemyGoal::CircleLeft,
        EnemyGoal::CircleRight,
        EnemyGoal::Retreat,
    ] {
        for delta in [1, 2] {
            let mut e = engaged();
            e.begin_goal(0, goal, 240, 96);
            for _ in 0..(12 / delta) {
                let before = e.position(0);
                e.x[0] -= i32::from(delta);
                e.advance_tactical_animation(&r, 0, before, delta);
            }
            // These clips already encode the slower cadence in their sample rate.
            // Movement at their nominal speed must not halve playback a second time.
            assert_eq!((e.tactics[0].animation_phase_q8 >> 8) as u16, 12);
        }
    }
}

#[test]
fn two_cannon_attacks_lead_to_a_close_approach_after_spacing() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.combat_flags[0] |= GAME_ENTITY_STANCE_ZENITH;
    e.tactics[0].last_attack = GAME_ENTITY_ATTACK_RANGED;
    e.tactics[0].repeat_count = 2;
    e.attack_cooldown[0] = 240;
    let mut approached = false;
    for _ in 0..100 {
        tick(&mut e, &mut m, [0, 0, 128], 2);
        approached |= e.stance(0) == VitalityChannelId::One && e.z[0] >= 60;
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
    e.x[0] = 36; // Returned from the detour to the same obstructing wall.
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
        e.stance_swap_cooldown[0] = 600;
        e.remember_target(0, [0, 0, 30]);
        e.begin_goal(0, EnemyGoal::Retreat, 210, 128);
        let mut arena = Arena::default();
        for elapsed in (delta..=12).step_by(usize::from(delta)) {
            e.tick_delta(records, input([0, 0, 30]), &mut arena, delta);
            if elapsed >= 2 {
                assert_eq!(e.clip_for_state(records, 0).clip, r.walk_backward_clip);
            }
        }
        assert_eq!(e.z[0], -6);
        assert_eq!(e.clip_for_state(records, 0).phase_ticks, 12);
        // A real collision still stops the gait; a deferred sub-unit step does not restart it.
        arena.blocked = true;
        for _ in 0..4 {
            e.tick_delta(records, input([0, 0, 30]), &mut arena, delta);
        }
        assert_eq!(e.clip_for_state(records, 0).clip, r.idle_clip);
    }
}

#[test]
fn terminal_presentation_advances_only_defeated_actors() {
    let mut entities = GameEntities::<2>::EMPTY;
    entities.spawn_from_records(&ENEMY);
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
        target.player_combat = Some((VitalityChannelId::Two, [100, 100]));
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
    e.enable_combat_flow(true);
    e.mutate_stance(0);
    e.advance_stance_swap(0, 12);
    e.stance_swap_cooldown[0] = 0;
    for _ in 0..5 {
        e.spend_shot_energy(0);
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
    e.stance_swap_cooldown[0] = 0;
    tick(&mut e, &mut Arena::default(), [0, 200, 140], 2);
    assert_eq!(e.stance(0), VitalityChannelId::One);
}

#[test]
fn empty_enemy_does_not_claw_at_an_overhead_perch_or_invent_energy() {
    let mut e = engaged();
    e.enable_combat_flow(true);
    for _ in 0..5 {
        e.spend_shot_energy(0);
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
    e.enable_combat_flow(true);
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
    e.enable_combat_flow(true);
    e.flow[0].melee_hit(true);
    let mut arena = Arena::default();
    let mut escaped = false;
    let mut attacked = false;
    for _ in 0..300 {
        tick(&mut e, &mut arena, [0, 0, 26], 2);
        escaped |= e.tactics[0].goal == EnemyGoal::BreakAway;
        if e.state(0) == GameEntityState::Windup {
            assert!(!e.player_within(0, input([0, 0, 26]), 192));
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
    e.enable_combat_flow(true);
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
    e.enable_combat_flow(true);
    let edge = i32::from(ENEMY[0].aggro_radius);
    e.z[0] = edge;
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
    e.enable_combat_flow(true);
    let edge = i32::from(ENEMY[0].aggro_radius);
    e.z[0] = edge;
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
    assert!(e.step_ranged_exchange(&ENEMY[0], 0, input([0, 0, 224]), &mut m, 1, 256, &mut stats));
    assert!(e.x[0] > 0);
    assert_eq!(e.z[0], 0);
    let before = e.position(0);
    m.blocked = true;
    assert!(e.step_ranged_exchange(&ENEMY[0], 0, input([0, 0, 224]), &mut m, 1, 256, &mut stats));
    assert_eq!(e.position(0), before);
}
#[test]
fn visible_melee_recovery_releases_an_active_defensive_retreat() {
    let mut e = engaged();
    let mut m = Arena::default();
    let mut stats = GameEntityTickStats::default();
    e.set_player_attack_read(1);
    assert!(e.step_melee_defense(&ENEMY[0], 0, input([0, 0, 50]), &mut m, 1, &mut stats));
    assert_eq!(e.tactics[0].goal, EnemyGoal::Retreat);
    e.set_player_attack_read(3);
    assert!(!e.step_melee_defense(&ENEMY[0], 0, input([0, 0, 50]), &mut m, 1, &mut stats));
    assert_eq!(e.tactics[0].goal, EnemyGoal::None);
    assert_eq!(e.combat_role(0), 7);
}

#[test]
fn empty_energy_melee_enemy_sidesteps_with_lateral_clip_then_plants() {
    let mut e = engaged();
    let mut m = Arena::default();
    e.enable_combat_flow(true);
    e.flow[0].energy = 0;
    e.stance_swap_cooldown[0] = 6000;
    e.projectile_threats[0] = Some(crate::projectiles::ProjectileThreat {
        position: [0, 32, 160],
        velocity: [0, 0, -8],
        ticks_to_contact: 20,
    });
    tick(&mut e, &mut m, [0, 0, 224], 2);
    assert_eq!(e.tactics[0].goal, EnemyGoal::Evade);
    assert_eq!(e.intent(0), GameEntityIntent::CircleRight);
    assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].strafe_right_clip);
    assert_eq!(e.x[0], 8);
    assert_eq!(e.flow[0].energy, 0);
    e.projectile_threats[0] = None;
    for _ in 0..9 {
        tick(&mut e, &mut m, [0, 0, 224], 2);
    }
    let p = e.position(0);
    assert!(p[0] >= 64 && p[0] <= 72);
    assert_eq!(p[2], 0);
    tick(&mut e, &mut m, [0, 0, 224], 2);
    assert_eq!(e.position(0), p);
    assert_eq!(e.state(0), GameEntityState::Aggro);
    assert_eq!(e.clip_for_state(&ENEMY, 0).clip, ENEMY[0].idle_clip);
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
        e.enable_combat_flow(true);
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
