//! One mover: the player and every enemy move through the same motor, so
//! the same intent and the same character numbers move them alike, an enemy
//! rolls with the player's i-frames, and a wall stops both at one place.

use super::*;
use psx_engine::{CharacterBlockerTraceProvider, CharacterCollisionCylinder, CharacterMotorFrame};
use psx_game_runtime::entities::{FighterWorld, GameEntityTickStats, OpenSpace, OpenWorld};

/// An enemy with `flags` (on top of a living, striking fighter), alone in
/// its own entity table, and the motor configuration the player's motor
/// would run for the same character numbers.
fn fighter(
    flags: u16,
) -> (
    &'static [LevelGameEntityRecord],
    std::boxed::Box<RuntimeGameEntities>,
    CharacterMotorConfig,
) {
    let records: &'static [LevelGameEntityRecord] =
        std::boxed::Box::leak(std::boxed::Box::new([LevelGameEntityRecord {
            flags: psx_level::game_entity_flags::ENABLED
                | psx_level::game_entity_flags::MELEE_ATTACK
                | psx_level::game_entity_flags::LIGHT_ATTACK
                | flags,
            ..GAME_ENTITIES[0]
        }]));
    let mut entities = std::boxed::Box::new(RuntimeGameEntities::EMPTY);
    entities.spawn_from_records(records);
    entities.set_spatial_active_mask(u64::MAX);
    let r = &records[0];
    // The one builder both sides use (the player's goes through
    // RuntimeCharacter::motor_config, which calls it).
    let config = psx_game_runtime::character::motion_motor_config(
        i32::from(r.radius),
        i32::from(r.height),
        &r.motion,
        r.flags & psx_level::game_entity_flags::CAN_RUN != 0,
        (1, 1),
    );
    (records, entities, config)
}

/// A player motor standing where entity 0 stands, facing where it faces.
fn player_beside(entities: &RuntimeGameEntities) -> CharacterMotorState {
    let p = entities.position(0);
    CharacterMotorState::new(
        RoomPoint::new(p[0], p[1], p[2]),
        Angle::from_q12(entities.yaw(0) as u16),
    )
}

#[test]
fn the_same_intent_moves_the_player_and_an_enemy_alike() {
    let (records, mut enemy, config) = fighter(0);
    let mut player = player_beside(&enemy);
    let intent = CombatIntent {
        movement: CombatMove::Move {
            x: 2896,
            z: 2896,
            forward: 0,
        },
        ..CombatIntent::IDLE
    };
    for tick in 0..24 {
        let mut stats = GameEntityTickStats::default();
        let theirs = enemy.apply_intent(&records[0], 0, intent, &mut OpenWorld, 2, &mut stats);
        // The pad's path for the same intent: motor_input, the facing held
        // (an AI without a face point keeps its facing too).
        let input = motor_input(&intent, false, false, Some(player.yaw()));
        let hers = player
            .update_vblanks_with_trace_provider(&mut OpenSpace, input, config, 2)
            .expect("open space");
        assert_eq!(theirs, hers, "tick {tick}");
        assert_eq!(enemy.motor(0), Some(&player), "tick {tick}");
    }
    assert!(player.position().x > 0 && player.position().z > 0);
}

#[test]
fn an_enemy_rolls_with_the_players_i_frames() {
    let (records, mut enemy, config) = fighter(psx_level::game_entity_flags::MOBILITY);
    let mut player = player_beside(&enemy);
    let dodge = CombatIntent {
        movement: CombatMove::Move {
            x: 4096,
            z: 0,
            forward: 0,
        },
        pressed: CombatActions::of(CombatAction::Evade),
        ..CombatIntent::IDLE
    };
    let roll = &records[0].motion;
    let frames = u16::from(roll.roll_active_frames) + u16::from(roll.roll_recovery_frames);
    let mut protected = 0;
    for frame in 0..frames + 4 {
        let intent = if frame == 0 {
            dodge
        } else {
            CombatIntent::IDLE
        };
        // Asked before the frame runs, as every hit resolver asks.
        let player_protected = player.is_action_invulnerable(config);
        assert_eq!(
            enemy
                .defender(records, 0)
                .map(|defender| defender.action.dodge),
            Some(player_protected),
            "frame {frame}"
        );
        let mut stats = GameEntityTickStats::default();
        let theirs = enemy.apply_intent(&records[0], 0, intent, &mut OpenWorld, 1, &mut stats);
        let input = motor_input(
            &intent,
            false,
            intent.pressed(CombatAction::Evade),
            Some(player.yaw()),
        );
        let hers = player
            .update_vblanks_with_trace_provider(&mut OpenSpace, input, config, 1)
            .expect("open space");
        assert_eq!(theirs, hers, "frame {frame}");
        // The roll's own frames, from the one it starts on.
        protected += u16::from(theirs.invulnerable);
        if frame == 0 {
            assert_eq!(theirs.action, psx_engine::CharacterMotorAction::Roll);
        }
    }
    assert_eq!(protected, u16::from(roll.roll_invulnerable_frames));
    assert!(player.position().x > 0, "the roll moved her");
    assert!(enemy.motor(0).is_some_and(|m| m.action().is_idle()));
}

#[test]
fn an_enemy_without_the_mobility_kit_does_not_roll() {
    let (records, mut enemy, _) = fighter(0);
    let mut stats = GameEntityTickStats::default();
    let frame = enemy.apply_intent(
        &records[0],
        0,
        CombatIntent {
            movement: CombatMove::Move {
                x: 4096,
                z: 0,
                forward: 0,
            },
            pressed: CombatActions::of(CombatAction::Evade),
            ..CombatIntent::IDLE
        },
        &mut OpenWorld,
        1,
        &mut stats,
    );
    // It walks the way it asked; no roll, no i-frames.
    assert!(frame.action.is_idle() && frame.moved);
    assert!(!enemy.defender(records, 0).unwrap().action.dodge);
}

/// Open space with one body standing in it.
struct OneBody {
    body: [CharacterCollisionCylinder; 1],
}

impl FighterWorld for OneBody {
    fn move_body(
        &mut self,
        _entity: usize,
        _room: RoomIndex,
        motor: &mut CharacterMotorState,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta: u16,
    ) -> CharacterMotorFrame {
        let mut space = OpenSpace;
        let mut provider = CharacterBlockerTraceProvider::new(&mut space, &self.body);
        motor
            .update_vblanks_with_trace_provider(&mut provider, input, config, delta)
            .expect("open space")
    }

    fn push_body(
        &mut self,
        _entity: usize,
        _room: RoomIndex,
        position: [i32; 3],
        _dx: i32,
        _dz: i32,
        _radius: i32,
        _height: i32,
    ) -> [i32; 3] {
        position
    }
}

#[test]
fn a_body_in_the_way_stops_the_player_and_an_enemy_at_the_same_place() {
    let (records, mut enemy, config) = fighter(0);
    let mut player = player_beside(&enemy);
    let start = enemy.position(0);
    let mut world = OneBody {
        body: [CharacterCollisionCylinder::new(
            RoomPoint::new(start[0] + 120, start[1], start[2]),
            14,
            77,
        )],
    };
    let intent = CombatIntent {
        movement: CombatMove::Move {
            x: 4096,
            z: 0,
            forward: 0,
        },
        ..CombatIntent::IDLE
    };
    let mut blocked = false;
    for tick in 0..60 {
        let mut stats = GameEntityTickStats::default();
        let theirs = enemy.apply_intent(&records[0], 0, intent, &mut world, 2, &mut stats);
        let input = motor_input(&intent, false, false, Some(player.yaw()));
        let mut space = OpenSpace;
        let mut provider = CharacterBlockerTraceProvider::new(&mut space, &world.body);
        let hers = player
            .update_vblanks_with_trace_provider(&mut provider, input, config, 2)
            .expect("open space");
        assert_eq!(theirs, hers, "tick {tick}");
        blocked |= hers.blocked;
    }
    let p = player.position();
    assert!(blocked, "the body stopped them");
    assert!(p.x < start[0] + 120 - 14, "short of the body: {p:?}");
    assert_eq!(enemy.position(0), [p.x, p.y, p.z]);
}

#[test]
fn the_motor_stays_small_enough_for_every_slot() {
    // 44 bytes per fighter: the RAM the one mover costs each enemy slot.
    assert_eq!(core::mem::size_of::<CharacterMotorState>(), 44);
}

/// Her scene, zeroed and initialised as the game does.
fn scene() -> std::boxed::Box<Playtest> {
    let layout = std::alloc::Layout::new::<Playtest>();
    // SAFETY: Playtest is plain data whose zero pattern init_zeroed expects.
    let raw = unsafe { std::alloc::alloc_zeroed(layout) as *mut Playtest };
    assert!(!raw.is_null());
    // SAFETY: raw is a fresh, zeroed, correctly sized allocation.
    unsafe { Playtest::init_zeroed(raw) };
    // SAFETY: allocated with the global allocator at Playtest's layout.
    unsafe { std::boxed::Box::from_raw(raw) }
}

#[test]
fn a_sub_unit_strafe_keeps_one_gait_on_one_clock_for_her_and_an_enemy() {
    let (_, _, config) = fighter(0);
    // An engaged tactical enemy whose locomotion clips are told apart.
    let records: &'static [LevelGameEntityRecord] =
        std::boxed::Box::leak(std::boxed::Box::new([LevelGameEntityRecord {
            flags: psx_level::game_entity_flags::ENABLED
                | psx_level::game_entity_flags::MELEE_ATTACK
                | psx_level::game_entity_flags::LIGHT_ATTACK
                | psx_level::game_entity_flags::TACTICAL,
            reaction_ticks: 0,
            idle_clip: 0,
            walk_clip: 1,
            walk_backward_clip: 2,
            strafe_left_clip: 3,
            strafe_right_clip: 4,
            ..GAME_ENTITIES[0]
        }]));
    // The tactical policy, whatever this fixture level's entities use.
    let mut enemy = std::boxed::Box::new(psx_game_runtime::entities::GameEntities::<2>::EMPTY);
    enemy.spawn_from_records(records);
    enemy.engage(0, psx_game_runtime::combatant::CombatantId::PLAYER);
    assert_eq!(
        enemy.state(0),
        psx_game_runtime::entities::GameEntityState::Aggro
    );
    let p = enemy.position(0);
    let mut her = CharacterMotorState::new(
        RoomPoint::new(p[0], p[1], p[2]),
        Angle::from_q12(enemy.yaw(0) as u16),
    );
    let mut scene = scene();
    // A slow side step: well under one unit per think at this walk.
    let intent = CombatIntent {
        movement: CombatMove::Move {
            x: 1200,
            z: 0,
            forward: 0,
        },
        ..CombatIntent::IDLE
    };
    let (mut her_whole_steps, mut her_still) = (0, 0);
    let mut last: Option<(u32, u16)> = None;
    for think in 0..30u32 {
        let now = SimTick::from_u32(1000 + 2 * think);
        let mut stats = GameEntityTickStats::default();
        let theirs = enemy.apply_intent(&records[0], 0, intent, &mut OpenWorld, 2, &mut stats);
        let input = motor_input(&intent, false, false, Some(her.yaw()));
        let before = her.position();
        let hers = her
            .update_vblanks_with_trace_provider(&mut OpenSpace, input, config, 2)
            .expect("open space");
        if her.position() == before {
            her_still += 1;
        } else {
            her_whole_steps += 1;
        }
        // Her selection: the shared mapping of the motor's intent, switched
        // on the shared track (a strafe has no windup phase).
        let anim = fighter_anim::locomotion_anim(hers.anim);
        if anim != scene.anim.state {
            scene.switch_player_anim(anim, now, VideoHz::NTSC);
        }
        // Theirs: the same mapping and track, at the presentation tick.
        enemy.update_animation(records, now, |_, _| 40);
        let shown = enemy.animation(0);
        assert_eq!(theirs.anim, hers.anim, "think {think}");
        if think == 0 {
            continue;
        }
        assert_eq!(scene.anim.state, PlayerAnim::StrafeLeft, "think {think}");
        assert_eq!(
            shown.clip.clip, records[0].strafe_left_clip,
            "think {think}"
        );
        let her_clock = now.saturating_sub(scene.game_entities.actor_action().start);
        if let Some((her_before, theirs_before)) = last {
            assert_eq!(her_clock, her_before + 2, "think {think}");
            assert_eq!(shown.clip.phase_ticks, theirs_before + 2, "think {think}");
        }
        last = Some((her_clock, shown.clip.phase_ticks));
    }
    assert!(
        her_whole_steps > 0 && her_still > 0,
        "{her_whole_steps} {her_still}"
    );
}
