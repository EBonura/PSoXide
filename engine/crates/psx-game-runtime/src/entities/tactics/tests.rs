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
                let expected_speed = if goal == EnemyGoal::Approach || percent == 100 { 2 } else { 1 };
                assert_eq!(e.tactics[0].requested, expected_speed * delta);
            }
            if goal == EnemyGoal::Approach {
                assert_eq!(displacements[0], displacements[1]);
            } else {
                // Integer Q12 steering may differ by one world unit.
                assert!((displacements[0] - 2 * displacements[1]).abs() <= 1, "{goal:?}, delta {delta}");
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
    for goal in [EnemyGoal::CircleLeft, EnemyGoal::CircleRight, EnemyGoal::Retreat] {
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
    let mut m = Arena { hidden: true, ..Default::default() };
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
