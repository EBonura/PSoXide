//! The verb contract on the player: what she may do follows from the kits
//! her character binds (the same `CombatKits` derivation enemies are cooked
//! with), and a strike she does not bind plays its documented stand-in.

use super::combat_baseline_tests::{character, scene};
use super::*;
use crate::playtest_update::player_strike;
use psx_level::OptionalModelClipIndex;

/// The fixture character with `action`'s clip removed.
fn without(mut c: RuntimeCharacter, action: CharacterAnimationAction) -> RuntimeCharacter {
    c.action_clips[action.to_index()] = OptionalModelClipIndex::NONE;
    c
}

#[test]
fn the_players_abilities_follow_her_kits() {
    let mut scene = scene();
    scene.character = Some(character(true));
    let kits = scene.player_kits();
    assert!(kits.can_melee() && kits.can_shoot() && kits.can_dodge() && kits.can_swap());
    assert!(scene.player_has_ranged_weapon());

    // No roll: no dodge, everything else stays.
    scene.character = Some(without(character(true), CharacterAnimationAction::Roll));
    let kits = scene.player_kits();
    assert!(!kits.can_dodge() && kits.can_swap());

    // No blade: no strike and no swap, though the cannon still shoots.
    let bladeless = without(
        without(character(true), CharacterAnimationAction::LightAttack),
        CharacterAnimationAction::HeavyAttack,
    );
    scene.character = Some(bladeless);
    let kits = scene.player_kits();
    assert!(!kits.can_melee() && kits.can_shoot() && !kits.can_swap());

    // No cannon: no shot, no swap. The aim pose alone is not the cannon.
    let mut aim_only = character(false);
    aim_only.action_clips[CharacterAnimationAction::RangedAim.to_index()] =
        OptionalModelClipIndex::from_raw(0);
    scene.character = Some(aim_only);
    assert!(!scene.player_kits().can_shoot() && !scene.player_has_ranged_weapon());

    // A model-less debug body binds nothing but keeps its dodge.
    scene.character = None;
    let kits = scene.player_kits();
    assert!(kits.can_dodge() && !kits.can_melee() && !kits.can_shoot());
}

#[test]
fn a_strike_she_does_not_bind_plays_its_stand_in_or_nothing() {
    use CharacterAnimationAction as A;
    let full = character(false);
    assert_eq!(
        player_strike(Some(&full), A::LightAttack),
        Some(PlayerAnim::LightAttack)
    );
    assert_eq!(
        player_strike(Some(&full), A::HeavyAttack),
        Some(PlayerAnim::HeavyAttack)
    );
    // A heavy with no clip is the light strike.
    let no_heavy = without(full, A::HeavyAttack);
    assert_eq!(
        player_strike(Some(&no_heavy), A::HeavyAttack),
        Some(PlayerAnim::LightAttack)
    );
    // The Zenith-axis strikes have no stand-in: unbound, the press is refused.
    assert_eq!(player_strike(Some(&full), A::VertLightAttack), None);
    // No blade at all: every strike is refused.
    let bladeless = without(without(full, A::LightAttack), A::HeavyAttack);
    assert_eq!(player_strike(Some(&bladeless), A::LightAttack), None);
    assert_eq!(player_strike(None, A::LightAttack), None);
}

#[test]
fn the_player_plays_each_missing_verbs_chain_never_another_clip() {
    use CharacterAnimationAction as A;
    let mut c = character(false);
    c.action_clips = [OptionalModelClipIndex::NONE; psx_level::CHARACTER_ANIMATION_ACTION_COUNT];
    let mut bind = |action: A, clip: u16| {
        c.action_clips[action.to_index()] = OptionalModelClipIndex::from_raw(clip);
    };
    bind(A::Idle, 1);
    bind(A::Walk, 2);
    bind(A::HitReact, 3);
    bind(A::LightAttack, 4);
    bind(A::Backstep, 5);
    bind(A::StrafeLeft, 6);
    let clip = |anim: PlayerAnim| c.clip_for(anim).to_usize();
    // Knockdown -> Stun -> HitReact.
    assert_eq!(clip(PlayerAnim::Knockdown), 3);
    assert_eq!(clip(PlayerAnim::Stun), 3);
    // Heavy -> Light.
    assert_eq!(clip(PlayerAnim::HeavyAttack), 4);
    // Roll -> Backstep; a dash -> Roll -> Backstep.
    assert_eq!(clip(PlayerAnim::Roll), 5);
    assert_eq!(clip(PlayerAnim::DashLeft), 5);
    // Ranged moves -> their plain move -> Walk.
    assert_eq!(clip(PlayerAnim::RangedLeft), 6);
    assert_eq!(clip(PlayerAnim::RangedRight), 2);
    assert_eq!(clip(PlayerAnim::Run), 2);
    // No stand-in: the rest pose, never some other bound clip.
    assert_eq!(clip(PlayerAnim::Death), 1);
    assert_eq!(clip(PlayerAnim::VertLightAttack), 1);
    assert_eq!(clip(PlayerAnim::RangedAim), 1);
}
