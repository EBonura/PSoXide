//! Hit feedback: a flash on the fighter that takes a hit and, for a melee
//! contact, a small spark, each pinned for both sides through the same
//! resolver. Nothing here is gameplay; a parry keeps its own look.

use super::combat_baseline_tests::{bolt, ctx, scene};
use super::combat_parry_tests::{duel, player_swaps, shot_at};
use super::*;
use crate::game_logic_runtime::CRIT_BOLT_RGB;
use psx_game_runtime::combat_fx::STAR_NO_RING;
use psx_game_runtime::projectiles::{CombatTeam, ProjectileDamageChannel};

/// The ringless sparks in the impact pool, as (colour, position, reach).
fn sparks(scene: &Playtest) -> std::vec::Vec<([u8; 3], [i32; 3], u16)> {
    (0..MAX_PROJECTILE_IMPACTS)
        .filter_map(|i| scene.combat_projectile_impacts.get(i))
        .filter(|effect| effect.visual.star_points & STAR_NO_RING != 0)
        .map(|effect| {
            (
                effect.visual.impact_rgb,
                effect.position,
                effect.visual.glow_scale_q8,
            )
        })
        .collect()
}

#[test]
fn an_enemy_bolt_that_lands_flashes_the_player_and_only_the_player() {
    let data = COMBAT_FEEDBACK;
    let mut scene = duel();
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Enemy))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    let flash = scene.player_flash;
    assert!(flash.is_active());
    assert_eq!(flash.kind(), FlashKind::Hit);
    // The Zenith bolt's channel is the attacker's colour.
    assert!(flash.is_zenith());
    assert_eq!(
        flash.ticks_left(),
        data.flash[FlashKind::Hit as usize].ticks
    );
    assert!(!scene.game_entities.any_flash(), "the attacker is not lit");
    assert!(sparks(&scene).is_empty(), "a bolt has its own impact");
}

#[test]
fn a_player_bolt_that_lands_flashes_the_enemy_and_only_the_enemy() {
    let mut scene = duel();
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Player))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    let flash = scene.game_entities.flash(0);
    assert!(flash.is_active());
    assert_eq!(flash.kind(), FlashKind::Hit);
    assert!(flash.is_zenith());
    assert!(!scene.player_flash.is_active(), "the shooter is not lit");
    assert!(sparks(&scene).is_empty());
}

#[test]
fn a_crit_bolt_flashes_harder_than_a_plain_one_on_either_side() {
    let data = COMBAT_FEEDBACK;
    for team in [CombatTeam::Enemy, CombatTeam::Player] {
        let mut plain = duel();
        plain.combat_projectiles.spawn(shot_at(team)).unwrap();
        plain.resolve_combat(&ctx(100));
        let mut crit = duel();
        let mut spawn = shot_at(team);
        spawn.visual.impact_rgb = CRIT_BOLT_RGB;
        crit.combat_projectiles.spawn(spawn).unwrap();
        crit.resolve_combat(&ctx(100));
        let (plain, crit) = if team == CombatTeam::Enemy {
            (plain.player_flash, crit.player_flash)
        } else {
            (plain.game_entities.flash(0), crit.game_entities.flash(0))
        };
        assert_eq!(plain.kind(), FlashKind::Hit);
        assert_eq!(crit.kind(), FlashKind::Crit);
        assert!(crit.weight_q8(&data) > plain.weight_q8(&data));
        assert!(crit.ticks_left() > plain.ticks_left());
    }
}

#[test]
fn a_melee_contact_sparks_halfway_between_the_fighters_for_either_side() {
    let data = COMBAT_FEEDBACK;
    let mut scene = duel();
    let player = scene.motor.position();
    let enemy = scene.game_entities.position(0);
    // The player's swing lands on the enemy (she is the attacker, Horizon).
    scene.show_hit(LandedHit {
        victim: CombatantId::entity(0),
        contact_from: Some([player.x, player.y, player.z]),
        kind: FlashKind::Hit,
        zenith: false,
    });
    assert!(scene.game_entities.flash(0).is_active());
    assert!(!scene.player_flash.is_active());
    let found = sparks(&scene);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, data.horizon_rgb);
    assert_eq!(found[0].1[0], enemy[0] / 2 + player.x / 2);
    assert_eq!(found[0].1[2], enemy[2] / 2 + player.z / 2);
    // The enemy's swing lands on the player (it is the attacker, Zenith).
    let mut scene = duel();
    scene.show_hit(LandedHit {
        victim: CombatantId::PLAYER,
        contact_from: Some(enemy),
        kind: FlashKind::Heavy,
        zenith: true,
    });
    assert!(scene.player_flash.is_active());
    assert!(scene.player_flash.is_zenith());
    assert!(!scene.game_entities.any_flash());
    let found = sparks(&scene);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, data.zenith_rgb);
    // Heavier is bigger, and any hit spark is smaller than the parry burst
    // and has no ring, so the two never look alike.
    let light = data.spark_style(FlashKind::Hit, false).glow_scale_q8;
    assert!(found[0].2 > light);
    assert!(found[0].2 < data.burst_reach_q8);
    assert!(data.spark_radius < data.burst_radius);
}

#[test]
fn a_parry_flash_is_not_replaced_by_a_hit_on_the_same_fighter() {
    let mut scene = duel();
    player_swaps(&mut scene);
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Enemy))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(scene.player_flash.kind(), FlashKind::Parry);
    let parry = scene.player_flash;
    // A hit on her while the parry still shows leaves it alone.
    scene.show_hit(LandedHit {
        victim: CombatantId::PLAYER,
        contact_from: None,
        kind: FlashKind::Crit,
        zenith: false,
    });
    assert_eq!(scene.player_flash, parry);
}

#[test]
fn a_parried_hit_shows_no_hit_flash_and_no_hit_spark() {
    // The enemy's bolt is parried by the player: she shows the parry, the
    // riposted enemy shows neither a hit flash nor a spark.
    let mut scene = duel();
    player_swaps(&mut scene);
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Enemy))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(scene.player_flash.kind(), FlashKind::Parry);
    assert!(!scene.game_entities.any_flash());
    assert!(sparks(&scene).is_empty());
    // And the other way round.
    let mut scene = duel();
    super::combat_parry_tests::enemy_swaps(&mut scene);
    for _ in 0..5 {
        scene.game_entities.spend_shot_energy(&GAME_ENTITIES[0], 0);
    }
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Player))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(scene.game_entities.flash(0).kind(), FlashKind::Parry);
    assert!(!scene.player_flash.is_active());
    assert!(sparks(&scene).is_empty());
}

#[test]
fn a_hit_flash_fades_out_and_a_bolt_with_no_owner_flashes_nobody_else() {
    let data = COMBAT_FEEDBACK;
    let mut scene = duel();
    scene
        .combat_projectiles
        .spawn(shot_at(CombatTeam::Enemy))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    for _ in 0..data.flash[FlashKind::Hit as usize].ticks {
        scene.resolve_combat(&ctx(101));
    }
    assert!(!scene.player_flash.is_active());
    // A world bolt (no owner) is no fighter's hit: nothing flashes.
    let mut scene = scene_with_world_bolt();
    scene.resolve_combat(&ctx(100));
    assert!(!scene.player_flash.is_active());
}

fn scene_with_world_bolt() -> std::boxed::Box<Playtest> {
    let mut scene = scene();
    let mut spawn = bolt(ProjectileDamageChannel::Horizon, 20, 0);
    spawn.owner = psx_game_runtime::projectiles::NO_PROJECTILE_OWNER;
    scene.combat_projectiles.spawn(spawn).unwrap();
    scene
}
