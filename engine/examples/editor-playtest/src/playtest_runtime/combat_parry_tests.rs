//! Parry, crit, knockdown and the swap rules, each pinned twice: once with
//! the player defending against the enemy and once the other way round.
//!
//! Both sides run through their real owners: the player through the
//! `Playtest` methods the scene calls, the enemy through `GameEntities` in
//! the same scene, on the fixture enemy record. Both hold the same
//! `Combatant` and the same rules; only their stats (data) differ.

use super::combat_baseline_tests::{bolt, character, ctx, scene, TEST_I_FRAMES};
use super::*;
use psx_game_runtime::combat_intent::{CombatAction, CombatActions, CombatIntent};
use psx_game_runtime::entities::{CombatTarget, GameEntityState, GameEntityTickInput, OpenWorld};
use psx_game_runtime::projectiles::{CombatTeam, ProjectileDamageChannel, NO_PROJECTILE_OWNER};
use psx_level::{game_entity_flags, LevelGameEntityRecord, StableId};

/// The fixture enemy, which every owner path reads its stats from.
fn enemy_record() -> &'static LevelGameEntityRecord {
    &GAME_ENTITIES[0]
}

/// The fixture enemy with `flags`, as its own static slice.
fn record_with_flags(flags: u16) -> &'static [LevelGameEntityRecord] {
    std::boxed::Box::leak(std::boxed::Box::new([LevelGameEntityRecord {
        flags,
        ..*enemy_record()
    }]))
}

/// A record with both a blade and a Zenith move: it may swap.
fn hybrid() -> &'static LevelGameEntityRecord {
    &record_with_flags(
        game_entity_flags::ENABLED
            | game_entity_flags::RANGED_ATTACK
            | game_entity_flags::MELEE_ATTACK,
    )[0]
}

/// A scene whose player carries the Zenith cannon (so she may swap) and
/// whose enemy slot 0 is alive, can swap, and stands at the origin with her.
pub(super) fn duel() -> std::boxed::Box<Playtest> {
    let mut scene = scene();
    scene.character = Some(character(true));
    assert!(scene.game_entities.spawn_slot(0, StableId(1), hybrid()));
    scene.game_entities.set_stance_swap_delay(30);
    scene.game_entities.set_spatial_active_mask(u64::MAX);
    scene
}

/// The swap press of one frame as the scene runs it: the swap, then the
/// clock advance of that same frame.
pub(super) fn player_swaps(scene: &mut Playtest) {
    let cooldown = scene.player_stance_config.swap_cooldown_ticks;
    assert!(scene.game_entities.actor_mut().request_swap(cooldown));
    scene.game_entities.actor_mut().advance_stance(1);
}

/// The enemy's swap in its tick, then the per-frame clock advance the scene
/// makes after the entity tick.
pub(super) fn enemy_swaps(scene: &mut Playtest) {
    assert!(scene.game_entities.swap_stance(hybrid(), 0));
    scene.game_entities.advance_stances(1);
}

/// A Zenith bolt fired by `team` (owner entity 0 for the enemy, none for the
/// player) already inside its target, the player at y 80 or the enemy at 40.
pub(super) fn shot_at(team: CombatTeam) -> psx_game_runtime::projectiles::ProjectileSpawn {
    let mut spawn = bolt(ProjectileDamageChannel::Zenith, 20, 40);
    spawn.team = team;
    if team == CombatTeam::Player {
        spawn.owner = NO_PROJECTILE_OWNER;
        spawn.position = [0, 40, 0];
    }
    spawn
}

fn far() -> GameEntityTickInput<'static> {
    GameEntityTickInput {
        target: CombatTarget {
            position: [100_000, 0, 100_000],
            radius: 40,
            height: 160,
            noise_radius: 0,
            defender: Defender::OPEN,
            stance: None,
            attack_read: 0,
            swing_ticks: None,
        },
    }
}

fn tick_enemy(scene: &mut Playtest, ticks: u16) {
    for _ in 0..ticks {
        scene
            .game_entities
            .tick(GAME_ENTITIES.as_slice(), far(), &mut OpenWorld);
    }
}

// ---------------------------------------------------------------------------
// Parry and its four effects.
// ---------------------------------------------------------------------------

#[test]
fn the_player_parries_an_enemy_bolt_early_in_her_swap() {
    let mut scene = duel();
    player_swaps(&mut scene);
    scene.game_entities.actor_mut().flow.energy = 0;
    assert_ne!(scene.game_entities.actor_mut().stance.swap_cooldown(), 0);
    let health = scene.game_entities.actor_mut().health();
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Enemy))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(
        scene.game_entities.actor().health(),
        health,
        "nothing landed"
    );
    // (a) the attacker is staggered through its own receive_hit.
    assert_eq!(scene.game_entities.state(0), GameEntityState::Staggered);
    // (b) the refund, (c) the cooldown, (d) the crit.
    let stats = scene.player_stats;
    assert_eq!(scene.game_entities.actor().flow.energy, stats.parry_refund);
    assert_eq!(scene.game_entities.actor().stance.swap_cooldown(), 0);
    assert_eq!(scene.game_entities.actor().crit, stats.crit_window_ticks);
    assert!(scene.combat_projectiles.is_empty(), "the bolt is spent");
}

#[test]
fn the_enemy_parries_a_player_bolt_early_in_its_swap() {
    let mut scene = duel();
    enemy_swaps(&mut scene);
    let record = enemy_record();
    for _ in 0..5 {
        scene.game_entities.spend_shot_energy(record, 0);
    }
    assert_eq!(scene.game_entities.energy(0), 0);
    let health = scene.game_entities.health(0);
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Player))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(scene.game_entities.health(0), health, "nothing landed");
    // (a) the attacker (the player) is staggered through her receive_hit:
    // a posture break, her stagger reaction, and no knockdown.
    assert_eq!(
        scene.game_entities.actor().flow.break_grace,
        BREAK_GRACE_TICKS_FOR_TEST
    );
    assert!(matches!(
        scene.anim_state,
        PlayerAnim::Stun | PlayerAnim::HitReact
    ));
    assert!(!scene.game_entities.actor().knocked_down());
    // (b) the refund, (c) the cooldown, (d) the crit.
    assert_eq!(scene.game_entities.energy(0), record.combat.parry_refund);
    assert_eq!(scene.game_entities.stance_swap_cooldown(0), 0);
    assert!(scene.game_entities.crit_armed(0));
}

const BREAK_GRACE_TICKS_FOR_TEST: u16 = psx_game_runtime::combat_flow::BREAK_GRACE_TICKS;

#[test]
fn a_parried_attacker_is_staggered_not_knocked_down_on_both_sides() {
    // The player's riposte on the enemy, and the enemy's on the player.
    let mut scene = duel();
    let outcome = scene
        .game_entities
        .riposte(GAME_ENTITIES.as_slice(), 0, CombatantId::PLAYER);
    assert!(outcome.staggered && !outcome.knocked_down);
    assert_eq!(scene.game_entities.state(0), GameEntityState::Staggered);
    assert!(scene.riposte_player(CombatantId::entity(0), &ctx(100)));
    assert!(!scene.game_entities.actor().knocked_down());
    assert_ne!(scene.anim_state, PlayerAnim::Knockdown);
}

#[test]
fn a_parried_attacker_plays_parried_on_both_sides() {
    use psx_level::{CharacterAnimationAction, OptionalModelClipIndex};
    // The enemy: its riposte stagger plays its cooked Parried clip; any other
    // stagger still plays the stagger clip. Same state, same timing.
    let records: &'static [LevelGameEntityRecord] =
        std::boxed::Box::leak(std::boxed::Box::new([LevelGameEntityRecord {
            parried_clip: 9,
            ..*hybrid()
        }]));
    let mut scene = duel();
    assert!(scene.game_entities.spawn_slot(0, StableId(1), &records[0]));
    let outcome = scene.game_entities.riposte(records, 0, CombatantId::PLAYER);
    assert!(outcome.staggered);
    assert_eq!(scene.game_entities.state(0), GameEntityState::Staggered);
    assert!(scene.game_entities.parried_stagger(0));
    assert_eq!(scene.game_entities.clip_for_state(records, 0).clip, 9);
    let mut plain = duel();
    assert!(plain.game_entities.spawn_slot(0, StableId(1), &records[0]));
    let broken = plain.game_entities.receive_hit(
        records,
        0,
        Strike {
            attacker: CombatantId::PLAYER,
            kind: HitKind::Melee,
            channel: VitalityChannelId::One,
            damage: 0,
            poise: u16::MAX,
            crit: false,
        },
    );
    assert!(broken.staggered || broken.knocked_down);
    assert!(!plain.game_entities.parried_stagger(0));
    assert_ne!(plain.game_entities.clip_for_state(records, 0).clip, 9);

    // The player: with a Parried clip she plays it over the stagger's lock;
    // without one she plays its chain (the stagger reaction) as before.
    let mut scene = duel();
    let mut aletha = character(true);
    aletha.action_clips[CharacterAnimationAction::Parried.to_index()] =
        OptionalModelClipIndex::from_raw(7);
    scene.character = Some(aletha);
    assert!(scene.riposte_player(CombatantId::entity(0), &ctx(100)));
    assert_eq!(scene.anim_state, PlayerAnim::Parried);
    assert_eq!(aletha.clip_for(PlayerAnim::Parried).to_usize(), 7);
    assert!(scene.game_entities.actor_action().lock_until > ctx(100).sim_tick);
    let mut scene = duel();
    assert!(scene.riposte_player(CombatantId::entity(0), &ctx(100)));
    assert_ne!(scene.anim_state, PlayerAnim::Parried);
}

#[test]
fn the_world_is_never_parried_on_either_side() {
    let mut scene = duel();
    player_swaps(&mut scene);
    let before = scene.game_entities.actor().health();
    assert!(!scene.apply_hazard_player_damage(10));
    assert_eq!(
        scene.game_entities.actor().health(),
        before - 10,
        "a hazard lands"
    );
    assert_eq!(scene.game_entities.actor().crit, 0);
    enemy_swaps(&mut scene);
    let before = scene.game_entities.health(0);
    let strike = Strike {
        attacker: CombatantId::WORLD,
        kind: HitKind::Hazard,
        channel: VitalityChannelId::One,
        damage: 10,
        poise: 0,
        crit: false,
    };
    let outcome = scene
        .game_entities
        .receive_hit(GAME_ENTITIES.as_slice(), 0, strike);
    assert!(!outcome.parried);
    assert!(scene.game_entities.health(0) < before);
    assert!(!scene.game_entities.crit_armed(0));
}

// ---------------------------------------------------------------------------
// A late swap-window hit does nothing.
// ---------------------------------------------------------------------------

#[test]
fn a_late_swap_window_hit_on_the_player_does_nothing() {
    let mut scene = duel();
    player_swaps(&mut scene);
    let parry = scene.player_stats.parry_ticks;
    scene.game_entities.actor_mut().advance_stance(parry);
    assert!(scene.player_invulnerable(&ctx(100)));
    let (health, energy) = (
        scene.game_entities.actor_mut().health(),
        scene.game_entities.actor_mut().flow.energy,
    );
    let cooldown = scene.game_entities.actor_mut().stance.swap_cooldown();
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Enemy))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(scene.game_entities.actor().health(), health);
    assert_eq!(scene.game_entities.actor().flow.energy, energy);
    assert_eq!(scene.game_entities.actor().stance.swap_cooldown(), cooldown);
    assert_eq!(scene.game_entities.actor().crit, 0);
    assert_ne!(scene.game_entities.state(0), GameEntityState::Staggered);
    assert_eq!(scene.combat_projectiles.len(), 1, "the bolt flew through");
}

#[test]
fn a_late_swap_window_hit_on_the_enemy_does_nothing() {
    let mut scene = duel();
    enemy_swaps(&mut scene);
    let parry = enemy_record().combat.parry_ticks;
    scene.game_entities.advance_stances(parry);
    assert!(scene
        .game_entities
        .invulnerable(GAME_ENTITIES.as_slice(), 0));
    let health = scene.game_entities.health(0);
    let cooldown = scene.game_entities.stance_swap_cooldown(0);
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Player))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(scene.game_entities.health(0), health);
    assert_eq!(scene.game_entities.stance_swap_cooldown(0), cooldown);
    assert!(!scene.game_entities.crit_armed(0));
    assert_eq!(
        scene.game_entities.actor().flow.break_grace,
        0,
        "no riposte"
    );
    assert_eq!(scene.combat_projectiles.len(), 1, "the bolt flew through");
}

// ---------------------------------------------------------------------------
// Crits: used up by the next attack, or expired.
// ---------------------------------------------------------------------------

/// A duel whose player has a crit armed through a parry.
fn player_crit_armed() -> std::boxed::Box<Playtest> {
    let mut scene = duel();
    player_swaps(&mut scene);
    scene.parry_on_player(
        Strike {
            attacker: CombatantId::entity(0),
            kind: HitKind::Melee,
            channel: VitalityChannelId::One,
            damage: 10,
            poise: 10,
            crit: false,
        },
        &ctx(100),
    );
    assert_ne!(scene.game_entities.actor().crit, 0);
    scene
}

/// A duel whose enemy has a crit armed through a parry.
fn enemy_crit_armed() -> std::boxed::Box<Playtest> {
    let mut scene = duel();
    enemy_swaps(&mut scene);
    let outcome =
        scene
            .game_entities
            .apply_hit(GAME_ENTITIES.as_slice(), 0, VitalityChannelId::One, 10, 10);
    assert!(outcome.parried && !outcome.connected);
    assert!(scene.game_entities.crit_armed(0));
    scene
}

fn swing(hits: u16) -> psx_game_runtime::entities::MeleeArcStats {
    psx_game_runtime::entities::MeleeArcStats {
        hits,
        ..Default::default()
    }
}

#[test]
fn a_crit_doubles_the_next_attack_and_is_used_up_by_it_on_both_sides() {
    let mut scene = player_crit_armed();
    let stats = scene.player_stats;
    // Armed, but nothing started yet: nothing doubles.
    assert_eq!(
        scene.game_entities.actor().crit_scaled(&stats, 10, 7),
        (10, 7)
    );
    // The player's next swing: doubled, then used up by the swing.
    scene.game_entities.actor_mut().start_attack();
    assert_eq!(
        scene.game_entities.actor_mut().crit_scaled(&stats, 10, 7),
        (20, 14)
    );
    scene.spend_player_melee_crit(swing(1), &ctx(101));
    assert_eq!(scene.game_entities.actor_mut().crit, 0);
    assert_eq!(
        scene.game_entities.actor().crit_scaled(&stats, 10, 7),
        (10, 7)
    );
    // The enemy's next attack, the same rule over its own stats.
    let mut scene = enemy_crit_armed();
    let records = GAME_ENTITIES.as_slice();
    assert_eq!(scene.game_entities.crit_scaled(records, 0, 10, 7), (10, 7));
    // Its next attack starts (a shot: the fixture has no strike clips).
    let mut stats = psx_game_runtime::entities::GameEntityTickStats::default();
    scene.game_entities.apply_intent(
        hybrid(),
        0,
        CombatIntent {
            pressed: CombatActions::of(CombatAction::Shoot),
            aim: Some([0, 0, 100]),
            ..CombatIntent::IDLE
        },
        &mut OpenWorld,
        2,
        &mut stats,
    );
    assert!(scene.game_entities.attack_is_crit(0));
    assert_eq!(scene.game_entities.crit_scaled(records, 0, 10, 7), (20, 14));
    assert!(scene.game_entities.spend_crit(0));
    assert!(!scene.game_entities.crit_armed(0));
    assert_eq!(scene.game_entities.crit_scaled(records, 0, 10, 7), (10, 7));
}

#[test]
fn a_swing_that_reaches_no_one_keeps_the_crit() {
    let mut scene = player_crit_armed();
    scene.spend_player_melee_crit(swing(0), &ctx(101));
    assert_ne!(scene.game_entities.actor().crit, 0);
}

#[test]
fn an_unused_crit_expires_after_its_window_on_both_sides() {
    let mut scene = player_crit_armed();
    let window = scene.player_stats.crit_window_ticks;
    for _ in 1..window {
        scene.game_entities.actor_mut().tick(1, false, false);
    }
    assert_ne!(scene.game_entities.actor_mut().crit, 0);
    scene.game_entities.actor_mut().tick(1, false, false);
    assert_eq!(scene.game_entities.actor_mut().crit, 0);
    // The enemy parried inside its swap: its window waits for the swap
    // animation to end, then runs for the same ticks as the player's.
    let mut scene = enemy_crit_armed();
    let window = enemy_record().combat.crit_window_ticks;
    let mut swap_ticks = 0;
    while scene.game_entities.stance_swap_in_progress(0) {
        tick_enemy(&mut scene, 1);
        swap_ticks += 1;
    }
    assert!(swap_ticks > 0);
    tick_enemy(&mut scene, window - 1);
    assert!(scene.game_entities.crit_armed(0));
    tick_enemy(&mut scene, 1);
    assert!(!scene.game_entities.crit_armed(0));
}

// ---------------------------------------------------------------------------
// Knockdown on a poise break, and getting up.
// ---------------------------------------------------------------------------

#[test]
fn a_poise_break_knocks_the_player_down_and_she_gets_up() {
    let mut scene = duel();
    let down = scene.player_stats.knockdown_ticks;
    let capacity = scene.player_stats.poise;
    assert!(scene.react_player_to_hit(
        CombatantId::entity(0),
        capacity,
        false,
        false,
        false,
        &ctx(100)
    ));
    assert!(scene.game_entities.actor().knocked_down());
    assert_eq!(scene.anim_state, PlayerAnim::Knockdown);
    assert_eq!(
        scene.game_entities.actor_action().lock_until,
        SimTick::from_u32(100 + u32::from(down)),
        "locked out of every action and all movement while down"
    );
    // Down: a hit lands but cannot break her again or restart a reaction.
    let health = scene.game_entities.actor().health();
    assert!(!scene.react_player_to_hit(
        CombatantId::entity(0),
        1000,
        false,
        false,
        false,
        &ctx(101)
    ));
    assert!(!scene.player_take_damage(CombatantId::entity(0), HitKind::Melee, None, 5));
    assert_eq!(scene.game_entities.actor().health(), health - 5);
    assert_eq!(scene.anim_state, PlayerAnim::Knockdown);
    for _ in 1..down {
        scene.game_entities.actor_mut().tick(1, true, false);
    }
    assert!(scene.game_entities.actor_mut().knocked_down());
    scene.game_entities.actor_mut().tick(1, true, false);
    assert!(
        !scene.game_entities.actor_mut().knocked_down(),
        "up after her knockdown ticks"
    );
}

#[test]
fn a_poise_break_knocks_the_enemy_down_and_it_gets_up() {
    let mut scene = duel();
    let record = enemy_record();
    let down = record.combat.knockdown_ticks;
    let outcome = scene.game_entities.apply_hit(
        GAME_ENTITIES.as_slice(),
        0,
        VitalityChannelId::One,
        1,
        record.combat.poise,
    );
    assert!(outcome.knocked_down);
    assert_eq!(scene.game_entities.state(0), GameEntityState::KnockedDown);
    // Down: a hit lands but cannot break it again.
    let health = scene.game_entities.health(0);
    let again =
        scene
            .game_entities
            .apply_hit(GAME_ENTITIES.as_slice(), 0, VitalityChannelId::One, 5, 1000);
    assert!(again.connected && !again.staggered);
    assert_eq!(scene.game_entities.health(0), health - 5);
    // It does not move while down, even with the player in reach.
    let near = GameEntityTickInput {
        target: CombatTarget {
            position: [60, 0, 0],
            ..far().target
        },
    };
    let at = scene.game_entities.position(0);
    for _ in 1..down {
        scene
            .game_entities
            .tick(GAME_ENTITIES.as_slice(), near, &mut OpenWorld);
        assert_eq!(scene.game_entities.state(0), GameEntityState::KnockedDown);
    }
    assert_eq!(scene.game_entities.position(0), at);
    scene
        .game_entities
        .tick(GAME_ENTITIES.as_slice(), near, &mut OpenWorld);
    assert_eq!(scene.game_entities.state(0), GameEntityState::Aggro);
}

// ---------------------------------------------------------------------------
// Who may swap, and the swap window's length.
// ---------------------------------------------------------------------------

#[test]
fn a_fighter_without_both_moves_never_swaps() {
    // An enemy with only a Zenith move starts and stays in Zenith.
    let ranged_only =
        record_with_flags(game_entity_flags::ENABLED | game_entity_flags::RANGED_ATTACK);
    let mut entities = RuntimeGameEntities::EMPTY;
    entities.spawn_from_records(ranged_only);
    entities.set_stance_swap_delay(30);
    entities.set_spatial_active_mask(u64::MAX);
    assert_eq!(entities.stance(0), VitalityChannelId::Two);
    assert!(!entities.swap_stance(&ranged_only[0], 0));
    for player in [[100_000, 0, 100_000], [40, 0, 0], [200, 0, 0]] {
        for _ in 0..120 {
            entities.tick_delta(
                ranged_only,
                GameEntityTickInput {
                    target: CombatTarget {
                        position: player,
                        ..far().target
                    },
                },
                &mut OpenWorld,
                2,
            );
            assert_eq!(entities.stance(0), VitalityChannelId::Two);
            assert_eq!(entities.stance_swap_cooldown(0), 0, "never swapped");
        }
    }
    // A melee-only enemy (the fixture) holds the blade the same way.
    let mut melee = duel();
    assert!(!melee.game_entities.swap_stance(enemy_record(), 0));
    // The player without a Zenith cannon has one kind of move too.
    let mut scene = scene();
    scene.character = Some(character(false));
    assert!(!scene.player_kits().can_swap());
    scene.character = Some(character(true));
    assert!(scene.player_kits().can_swap());
}

#[test]
fn the_swap_and_parry_windows_are_exactly_each_characters_count() {
    // The player: her press, then each frame's clock advance, as resolvers
    // then see it.
    let mut scene = duel();
    player_swaps(&mut scene);
    let (mut protected, mut parrying) = (0u16, 0u16);
    for _ in 0..100 {
        let defender = scene.player_defender(&ctx(100));
        protected += u16::from(defender.invulnerable());
        parrying += u16::from(defender.parries());
        scene.game_entities.actor_mut().advance_stance(1);
    }
    assert_eq!(protected, u16::from(TEST_I_FRAMES));
    assert_eq!(parrying, scene.player_stats.parry_ticks);
    // The enemy: its swap in its (30 Hz) tick, then the same per-frame
    // advance.
    enemy_swaps(&mut scene);
    let (mut protected, mut parrying) = (0u16, 0u16);
    for _ in 0..100 {
        let defender = scene
            .game_entities
            .defender(GAME_ENTITIES.as_slice(), 0)
            .unwrap();
        protected += u16::from(defender.invulnerable());
        parrying += u16::from(defender.parries());
        scene.game_entities.advance_stances(1);
    }
    let record = enemy_record();
    assert_eq!(protected, u16::from(record.motion.roll_invulnerable_frames));
    assert_eq!(parrying, record.combat.parry_ticks);
}

// ---------------------------------------------------------------------------
// Parry feedback: a flash on the parrier's model and a burst at the contact,
// the same for either side, in the parrier's stance colour.
// ---------------------------------------------------------------------------

/// The burst effects live in the impact pool, as (impact colour, position).
fn bursts(scene: &Playtest) -> std::vec::Vec<([u8; 3], [i32; 3])> {
    (0..MAX_PROJECTILE_IMPACTS)
        .filter_map(|i| scene.combat_projectile_impacts.get(i))
        .filter(|effect| {
            let points = effect.visual.star_points;
            points != 0 && points & psx_game_runtime::combat_fx::STAR_NO_RING == 0
        })
        .map(|effect| (effect.visual.impact_rgb, effect.position))
        .collect()
}

/// Both swaps of the player: Zenith first, then (past the cooldown) back to
/// Horizon, so a parry in her second swap is a Horizon parry.
fn player_swaps_back_to_horizon(scene: &mut Playtest) {
    player_swaps(scene);
    scene.game_entities.actor_mut().advance_stance(10_000);
    player_swaps(scene);
    assert_eq!(
        scene.game_entities.actor_mut().stance.active(),
        VitalityChannelId::One
    );
}

#[test]
fn the_players_parry_flashes_her_and_bursts_in_her_stance_colour() {
    let data = COMBAT_FEEDBACK;
    for horizon in [false, true] {
        let mut scene = duel();
        if horizon {
            player_swaps_back_to_horizon(&mut scene);
        } else {
            player_swaps(&mut scene);
        }
        assert!(!scene.player_flash.is_active());
        assert!(bursts(&scene).is_empty(), "nothing before the parry");
        scene
            .combat_projectiles
            .spawn(shot_at(CombatTeam::Enemy))
            .unwrap();
        scene.resolve_combat(&ctx(100));
        let rgb = data.stance_rgb(!horizon);
        // The flash is on her, in her colour; the enemy she parried is not lit.
        assert!(scene.player_flash.is_active());
        assert_eq!(scene.player_flash.is_zenith(), !horizon);
        assert_eq!(
            scene.player_flash.lit_tint(&data, None).unwrap().color,
            (rgb[0] / 2, rgb[1] / 2, rgb[2] / 2),
            "a silhouette: the all-white palette times half the colour"
        );
        assert!(scene.player_flash.solid(&data));
        assert!(!scene.game_entities.flash(0).is_active());
        // One burst, in her colour, between her and the attacker.
        let found = bursts(&scene);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, rgb);
        let player = scene.game_entities.actor_motor().position();
        let enemy = scene.game_entities.position(0);
        assert_eq!(found[0].1[0], player.x / 2 + enemy[0] / 2);
        assert_eq!(found[0].1[2], player.z / 2 + enemy[2] / 2);
    }
}

#[test]
fn the_enemys_parry_flashes_it_and_bursts_in_its_stance_colour() {
    let data = COMBAT_FEEDBACK;
    let mut scene = duel();
    enemy_swaps(&mut scene);
    let zenith = scene.game_entities.stance(0) == VitalityChannelId::Two;
    assert!(
        zenith,
        "the fixture enemy starts in Horizon and swaps to Zenith"
    );
    for _ in 0..5 {
        scene.game_entities.spend_shot_energy(enemy_record(), 0);
    }
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Player))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    let flash = scene.game_entities.flash(0);
    let rgb = data.stance_rgb(zenith);
    assert!(flash.is_active() && flash.is_zenith());
    assert_eq!(
        flash.lit_tint(&data, None).unwrap().color,
        (rgb[0] / 2, rgb[1] / 2, rgb[2] / 2)
    );
    assert!(flash.solid(&data));
    assert!(!scene.player_flash.is_active(), "the player is not lit");
    let found = bursts(&scene);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, rgb);
}

#[test]
fn a_flash_fades_out_and_a_hit_that_is_not_a_parry_shows_neither_tell() {
    let data = COMBAT_FEEDBACK;
    // The flash ends after its ticks; one resolve_combat is one tick.
    let mut scene = duel();
    player_swaps(&mut scene);
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Enemy))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    let ticks = data.flash[FlashKind::Parry as usize].ticks;
    assert_eq!(scene.player_flash.ticks_left(), ticks);
    assert_eq!(scene.player_flash.kind(), FlashKind::Parry);
    for _ in 0..ticks {
        scene.resolve_combat(&ctx(101));
    }
    assert!(!scene.player_flash.is_active());
    // A swap-window hit past the parry window is no parry: it is spent on the
    // swap's i-frames, so no flash and no burst (nothing landed).
    let mut scene = duel();
    player_swaps(&mut scene);
    let parry = scene.player_stats.parry_ticks;
    scene.game_entities.actor_mut().advance_stance(parry);
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Enemy))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert!(!scene.player_flash.is_active());
    assert!(bursts(&scene).is_empty());
    let mut scene = duel();
    enemy_swaps(&mut scene);
    let parry = enemy_record().combat.parry_ticks;
    scene.game_entities.advance_stances(parry);
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Player))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert!(!scene.game_entities.any_flash());
    assert!(bursts(&scene).is_empty());
}

/// A crit lands on the fighter its parry staggered, whose posture is
/// blocked by the riposte. Its posture damage counts anyway and knocks that
/// fighter down; the same hit without the crit leaves the stagger as it is.
/// Both sides.
#[test]
fn a_crit_knocks_down_the_riposted_fighter_on_both_sides() {
    let records = GAME_ENTITIES.as_slice();
    let blow = |crit: bool, poise: u16| Strike {
        attacker: CombatantId::PLAYER,
        kind: HitKind::Melee,
        channel: VitalityChannelId::One,
        damage: 1,
        poise,
        crit,
    };
    // The player's crit on the enemy her parry staggered.
    for crit in [false, true] {
        let mut scene = player_crit_armed();
        assert_eq!(scene.game_entities.state(0), GameEntityState::Staggered);
        let poise = enemy_record().combat.poise;
        let outcome = scene
            .game_entities
            .receive_hit(records, 0, blow(crit, poise));
        assert!(outcome.connected);
        assert_eq!(outcome.knocked_down, crit, "crit {crit}");
        assert_eq!(
            scene.game_entities.state(0),
            if crit {
                GameEntityState::KnockedDown
            } else {
                GameEntityState::Staggered
            }
        );
    }
    // The enemy's crit on the player its parry staggered.
    for crit in [false, true] {
        let mut scene = enemy_crit_armed();
        assert!(scene.riposte_player(CombatantId::entity(0), &ctx(100)));
        let poise = scene.player_stats.poise;
        let broke =
            scene.react_player_to_hit(CombatantId::entity(0), poise, false, false, crit, &ctx(101));
        assert_eq!(broke, crit, "crit {crit}");
        assert_eq!(scene.game_entities.actor().knocked_down(), crit);
    }
}
