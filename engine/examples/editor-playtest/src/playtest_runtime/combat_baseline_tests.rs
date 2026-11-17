//! Characterisation tests for the player side of combat.
//!
//! These pin what the code does TODAY, so the combat unification can prove it
//! changed nothing (or exactly what it meant to change). They are not a
//! statement of the target design: where the code differs from the
//! symmetric-rules target (swap i-frames that exist for the player alone) the
//! test asserts the current behaviour and says so. Step 5 made health one
//! bar: the two-pool tests were rewritten to pin that rule. When a later step changes a
//! number or a rule on purpose, the failing test names the behaviour that
//! moved.
//!
//! Everything here drives the same `Playtest` methods the scene calls, on the
//! fixture manifest and, where a rule depends on the character record
//! (ranged weapon, body size), on a record built in the test.

use super::*;
use psx_game_runtime::combat_flow::{CombatFlow, AIR_TICKS, BREAK_GRACE_TICKS};
use psx_game_runtime::entities::MeleeArcStats;
use psx_game_runtime::projectiles::{
    CombatTeam, ProjectileDamageChannel, ProjectileSpawn, ProjectileVisualStyle,
};
use psx_level::{
    CharacterActionFrameRange, CharacterActionPush, CharacterAnimationAction,
    CharacterCombatWindow, CombatCapsuleIndex, CombatWindowKind, LevelCharacterRecord, ModelIndex,
    OptionalModelClipIndex, CHARACTER_ANIMATION_ACTION_COUNT, MAX_CHARACTER_COMBAT_WINDOWS,
};

/// The player character's combat numbers when nothing else is cooked
/// (`CombatStats::PLAYER`). Their names are the constants they replaced.
const PLAYER_MAX_HEALTH: u16 = CombatStats::PLAYER.max_health;
const PLAYER_POISE: u16 = CombatStats::PLAYER.poise;
const ENERGY_MAX: u16 = CombatStats::PLAYER.energy_max;
const SHOT_COST: u16 = CombatStats::PLAYER.shot_cost;
const MELEE_GAIN: u16 = CombatStats::PLAYER.light_gain;
const HEAVY_GAIN: u16 = CombatStats::PLAYER.heavy_gain;

/// Roll / backstep invulnerable frames of the test character.
pub(super) const TEST_I_FRAMES: u8 = 12;

pub(super) fn scene() -> std::boxed::Box<Playtest> {
    let layout = std::alloc::Layout::new::<Playtest>();
    let raw = unsafe { std::alloc::alloc_zeroed(layout) as *mut Playtest };
    assert!(!raw.is_null());
    unsafe { Playtest::init_zeroed(raw) };
    unsafe { std::boxed::Box::from_raw(raw) }
}

/// A context at `tick` on NTSC. The engine only builds a `Ctx` inside its
/// app runner; every field this scene reads in the tested paths is public
/// and set here, and the private ones (request flags, port tokens) are plain
/// integers and `None`, so the zero pattern is a valid value.
pub(super) fn ctx(tick: u32) -> Ctx {
    let mut ctx: Ctx = unsafe { std::mem::MaybeUninit::<Ctx>::zeroed().assume_init() };
    ctx.sim_tick = SimTick::from_u32(tick);
    ctx.video_hz = VideoHz::NTSC;
    ctx
}

/// A character record with the given body and, optionally, the two clips that
/// make `player_has_ranged_weapon` true. No model is cooked, so no clip
/// resolves to an animation: reactions fall back exactly like an unbound
/// project.
pub(super) fn character(ranged: bool) -> RuntimeCharacter {
    let mut clips = [OptionalModelClipIndex::NONE; CHARACTER_ANIMATION_ACTION_COUNT];
    // The blade and the roll every playable character binds; the cannon is
    // the variable.
    for action in [
        CharacterAnimationAction::LightAttack,
        CharacterAnimationAction::HeavyAttack,
        CharacterAnimationAction::Roll,
    ] {
        clips[action.to_index()] = OptionalModelClipIndex::from_raw(0);
    }
    if ranged {
        for action in [
            CharacterAnimationAction::RangedAttack,
            CharacterAnimationAction::RangedAim,
        ] {
            clips[action.to_index()] = OptionalModelClipIndex::from_raw(0);
        }
    }
    let record = LevelCharacterRecord {
        model: ModelIndex::ZERO,
        action_clips: clips,
        action_flags: [0; CHARACTER_ANIMATION_ACTION_COUNT],
        action_speeds: [256; CHARACTER_ANIMATION_ACTION_COUNT],
        action_frame_ranges: [CharacterActionFrameRange::FULL; CHARACTER_ANIMATION_ACTION_COUNT],
        action_pushes: [CharacterActionPush::NONE; CHARACTER_ANIMATION_ACTION_COUNT],
        combat_windows: [CharacterCombatWindow::NONE; MAX_CHARACTER_COMBAT_WINDOWS],
        combat_capsule_first: CombatCapsuleIndex::ZERO,
        combat_capsule_count: 0,
        visual_offset: [0; 3],
        visual_yaw: 0,
        visual_scale_q8: 256,
        weight_q8: 256,
        radius: 40,
        height: 160,
        walk_speed: 256,
        run_speed: 512,
        turn_speed_degrees_per_second: 180,
        stamina_max_q12: 4096,
        sprint_min_q12: 0,
        sprint_drain_q12: 0,
        stamina_recover_q12: 0,
        roll_cost_q12: 0,
        roll_speed: 512,
        roll_active_frames: 20,
        roll_recovery_frames: 10,
        roll_invulnerable_frames: TEST_I_FRAMES,
        combat: CombatStats::PLAYER,
        stance_swap_cooldown_ticks: 30,
        stance_swap_duration_ticks: 12,
        backstep_cost_q12: 0,
        backstep_speed: 256,
        backstep_active_frames: 10,
        backstep_recovery_frames: 10,
        backstep_invulnerable_frames: TEST_I_FRAMES,
        camera_distance: 0,
        camera_height: 0,
        camera_target_height: 0,
        material_override: None,
        flags: 0,
    };
    crate::character_runtime::runtime_character_from_record(&record)
}

fn health(scene: &Playtest) -> u16 {
    scene.game_entities.actor().health()
}

pub(super) fn bolt(channel: ProjectileDamageChannel, damage: u16, poise: u16) -> ProjectileSpawn {
    ProjectileSpawn {
        // Dead centre of the test character's body, already inside it, so
        // the first sweep connects without needing a world.
        position: [0, 80, 0],
        velocity: [1, 0, 0],
        radius: 4,
        damage,
        poise_damage: poise,
        lifetime_ticks: 10,
        room: RoomIndex::ZERO,
        team: CombatTeam::Enemy,
        owner: 0,
        tint_rgb: [255; 3],
        damage_channel: channel,
        visual: ProjectileVisualStyle::EMPTY,
    }
}

// ---------------------------------------------------------------------------
// Hit routing: every hit comes off the one health bar, scaled by stance.
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_player_starts_with_a_full_bar_in_the_horizon_stance() {
    let scene = scene();
    assert_eq!(PLAYER_MAX_HEALTH, 200, "the two old 100 pools as one bar");
    assert_eq!(health(&scene), PLAYER_MAX_HEALTH);
    assert_eq!(
        scene.game_entities.actor().stance.active(),
        VitalityChannelId::One
    );
    assert_eq!(scene.player_stance_config, CombatStanceConfig::DEFAULT);
    assert_eq!(scene.game_entities.actor().flow, CombatFlow::FULL);
    assert_eq!(scene.hazard_death_ticks_remaining, 0);
}

#[test]
fn an_untyped_hit_counts_as_matching_and_lands_as_authored() {
    let mut scene = scene();
    assert!(!scene.apply_untyped_player_damage(20));
    assert_eq!(health(&scene), 180);
}

#[test]
fn an_untyped_hit_matches_whichever_stance_is_active() {
    let mut scene = scene();
    let config = scene.player_stance_config;
    assert!(scene.game_entities.actor_mut().stance.request_swap(&config));
    assert!(!scene.apply_untyped_player_damage(20));
    assert_eq!(health(&scene), 180);
}

#[test]
fn a_typed_hit_matching_the_active_channel_lands_as_authored() {
    let mut scene = scene();
    assert!(!scene.apply_typed_player_damage(ProjectileDamageChannel::Horizon, 20));
    assert_eq!(health(&scene), 180);
}

#[test]
fn a_typed_hit_on_the_other_channel_takes_a_quarter_extra() {
    // The one stance rule: an opposite-stance hit takes the defender's
    // opposite multiplier (1.25x to start), off the one bar.
    let mut scene = scene();
    assert!(!scene.apply_typed_player_damage(ProjectileDamageChannel::Zenith, 20));
    assert_eq!(health(&scene), 175);
}

#[test]
fn a_damaging_hit_queues_the_damage_cue_and_a_zero_hit_does_not() {
    let mut scene = scene();
    scene.apply_untyped_player_damage(0);
    assert_eq!(scene.gameplay_sfx_events, 0);
    scene.apply_untyped_player_damage(4);
    assert_eq!(
        scene.gameplay_sfx_events & LevelGameplaySfxEvent::PlayerDamage.bit(),
        LevelGameplaySfxEvent::PlayerDamage.bit()
    );
}

#[test]
fn a_ranged_weapon_character_takes_full_matching_and_a_quarter_extra_opposed_damage() {
    // The rule no longer depends on the inventory; the melee-only test below
    // gives the same numbers.
    let mut scene = scene();
    scene.character = Some(character(true));
    assert!(scene.player_has_ranged_weapon());
    assert!(!scene.apply_untyped_player_damage(20));
    assert_eq!(health(&scene), 180);
    assert!(!scene.apply_typed_player_damage(ProjectileDamageChannel::Zenith, 20));
    assert_eq!(health(&scene), 155);
}

#[test]
fn a_melee_only_character_runs_the_same_rule() {
    let mut scene = scene();
    scene.character = Some(character(false));
    assert!(!scene.player_has_ranged_weapon());
    assert!(!scene.apply_untyped_player_damage(20));
    assert_eq!(health(&scene), 180);
    assert!(!scene.apply_typed_player_damage(ProjectileDamageChannel::Zenith, 20));
    assert_eq!(health(&scene), 155);
}

#[test]
fn a_hit_bigger_than_the_bar_is_lethal_and_swaps_nothing() {
    let mut scene = scene();
    assert!(scene.apply_untyped_player_damage(1000));
    assert_eq!(health(&scene), 0);
    assert_eq!(
        scene.game_entities.actor().stance.active(),
        VitalityChannelId::One
    );
    assert_eq!(
        scene.game_entities.actor().stance.swap_elapsed_ticks(),
        u16::MAX
    );
    assert!(scene.game_entities.actor().defeated());
}

#[test]
fn only_emptying_the_bar_is_lethal() {
    let mut scene = scene();
    assert!(!scene.apply_untyped_player_damage(199));
    assert_eq!(health(&scene), 1);
    assert!(scene.apply_untyped_player_damage(1));
    assert!(scene.game_entities.actor().defeated());
}

#[test]
fn a_hazard_hit_bigger_than_an_old_pool_is_just_damage() {
    // 105 was past the old 100 pool (it emptied it, forced a swap and
    // carried 5 over); on the one bar it is 105 off 200.
    let mut scene = scene();
    assert!(!scene.apply_hazard_player_damage(105));
    assert_eq!(health(&scene), 95);
    assert_eq!(
        scene.game_entities.actor().stance.active(),
        VitalityChannelId::One
    );
}

#[test]
fn a_hazard_hit_bigger_than_the_bar_is_lethal() {
    let mut scene = scene();
    assert!(scene.apply_hazard_player_damage(PLAYER_MAX_HEALTH));
    assert_eq!(health(&scene), 0);
}

#[test]
fn a_hazard_hit_inside_the_bar_behaves_like_any_other_hit() {
    let mut scene = scene();
    assert!(!scene.apply_hazard_player_damage(10));
    assert_eq!(health(&scene), 190);
    assert_eq!(
        scene.game_entities.actor().stance.active(),
        VitalityChannelId::One
    );
}

#[test]
fn liquid_hazards_tick_on_their_own_rhythm() {
    assert_eq!(hazard_damage(psx_grid::contents::CONTENTS_SLIME, 30), 4);
    assert_eq!(hazard_damage(psx_grid::contents::CONTENTS_SLIME, 31), 0);
    assert_eq!(hazard_damage(psx_grid::contents::CONTENTS_LAVA, 15), 10);
    assert_eq!(hazard_damage(psx_grid::contents::CONTENTS_LAVA, 16), 0);
    assert_eq!(hazard_damage(psx_grid::contents::CONTENTS_WATER, 30), 0);
}

#[test]
fn a_slime_hazard_costs_four_health_per_hit() {
    // 4 raw hazard damage through the untyped route, as authored.
    let mut scene = scene();
    let tick = 30;
    let damage = hazard_damage(psx_grid::contents::CONTENTS_SLIME, tick);
    assert!(!scene.apply_untyped_player_damage(damage));
    assert_eq!(health(&scene), 196);
}

#[test]
fn an_enemy_bolt_routes_by_its_channel_through_the_resolver() {
    // End to end through `resolve_combat`: a Horizon bolt on a Horizon
    // stance matches (as authored), a Zenith bolt on the same stance is the
    // opposite stance (1.25x). The bolt starts inside the body capsule, so
    // one tick connects.
    for (channel, expected) in [
        (ProjectileDamageChannel::Horizon, 180),
        (ProjectileDamageChannel::Zenith, 175),
    ] {
        let mut scene = scene();
        scene.character = Some(character(false));
        scene
            .combat_projectiles
            .spawn(bolt(channel, 20, 40))
            .unwrap();
        scene.resolve_combat(&ctx(100));
        assert_eq!(health(&scene), expected, "{channel:?}");
        assert_eq!(scene.combat_projectiles.len(), 0, "the bolt is consumed");
    }
}

#[test]
fn an_enemy_bolt_does_not_touch_the_energy_bar() {
    let mut scene = scene();
    scene.character = Some(character(false));
    scene
        .combat_projectiles
        .spawn(bolt(ProjectileDamageChannel::Horizon, 20, 40))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(scene.game_entities.actor().flow, CombatFlow::FULL);
}

#[test]
fn an_unreadable_bolt_poise_is_a_quarter_capped_at_ten_and_never_breaks_the_player_alone() {
    // A bolt that arrives outside a melee opening deals `min(poise/4, 10)`
    // posture, far below the 60 capacity, so a lone bolt never staggers.
    let mut scene = scene();
    scene.character = Some(character(false));
    scene
        .combat_projectiles
        .spawn(bolt(ProjectileDamageChannel::Horizon, 20, 400))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(scene.game_entities.actor().flow.break_grace, 0);
    assert_ne!(
        scene.game_entities.actor().poise,
        psx_game_runtime::poise::Poise::EMPTY
    );
    assert_ne!(scene.anim_state, PlayerAnim::HitReact);
}

#[test]
fn a_player_inside_the_swap_window_is_not_a_projectile_target() {
    let mut scene = scene();
    scene.character = Some(character(false));
    let config = scene.player_stance_config;
    scene.game_entities.actor_mut().stance.request_swap(&config);
    // Past the parry window: the parry tests pin the early part.
    scene
        .game_entities
        .actor_mut()
        .advance_stance(scene.player_stats.parry_ticks + 1);
    assert!(scene.player_invulnerable(&ctx(100)));
    scene
        .combat_projectiles
        .spawn(bolt(ProjectileDamageChannel::Horizon, 20, 40))
        .unwrap();
    scene.resolve_combat(&ctx(100));
    assert_eq!(health(&scene), PLAYER_MAX_HEALTH);
    assert_eq!(scene.combat_projectiles.len(), 1, "the bolt flew through");
}

// ---------------------------------------------------------------------------
// Poise and stagger.
// ---------------------------------------------------------------------------

#[test]
fn the_player_poise_capacity_is_sixty() {
    assert_eq!(PLAYER_POISE, 60);
}

#[test]
fn poise_accumulates_then_breaks_and_the_break_starts_the_grace_window() {
    let mut scene = scene();
    assert!(!scene.react_player_to_hit(CombatantId::WORLD, 30, false, false, false, &ctx(100)));
    assert_eq!(scene.game_entities.actor().flow.break_grace, 0);
    assert!(scene.react_player_to_hit(CombatantId::WORLD, 30, false, false, false, &ctx(101)));
    assert_eq!(
        scene.game_entities.actor().flow.break_grace,
        BREAK_GRACE_TICKS
    );
    assert_eq!(
        scene.game_entities.actor().poise,
        psx_game_runtime::poise::Poise::EMPTY,
        "a break spends the accumulated posture"
    );
}

#[test]
fn a_break_without_a_stun_clip_falls_back_to_hit_react_with_an_eighteen_tick_lock() {
    let mut scene = scene();
    // Knockdown off: a break then staggers, which is what this pins.
    scene.player_stats.knockdown_ticks = 0;
    assert!(scene.react_player_to_hit(CombatantId::WORLD, 60, false, false, false, &ctx(100)));
    assert_eq!(scene.anim_state, PlayerAnim::HitReact);
    assert_eq!(
        scene.game_entities.actor_action().lock_until,
        SimTick::from_u32(118)
    );
}

#[test]
fn a_second_break_is_refused_until_the_grace_window_has_drained() {
    let mut scene = scene();
    // Knockdown off: the knockdown's own block is pinned elsewhere.
    scene.player_stats.knockdown_ticks = 0;
    assert!(scene.react_player_to_hit(CombatantId::WORLD, 60, false, false, false, &ctx(100)));
    scene.anim_state = PlayerAnim::Idle;
    assert!(!scene.react_player_to_hit(CombatantId::WORLD, 60, false, false, false, &ctx(101)));
    // Grace only drains once the reaction is over.
    scene
        .game_entities
        .actor_mut()
        .flow
        .tick(BREAK_GRACE_TICKS - 1, true);
    assert!(!scene.game_entities.actor_mut().flow.can_interrupt());
    scene
        .game_entities
        .actor_mut()
        .flow
        .tick(BREAK_GRACE_TICKS - 1, false);
    assert!(!scene.game_entities.actor_mut().flow.can_interrupt());
    scene.game_entities.actor_mut().flow.tick(1, false);
    assert!(scene.game_entities.actor_mut().flow.can_interrupt());
    assert!(scene.react_player_to_hit(CombatantId::WORLD, 60, false, false, false, &ctx(300)));
}

#[test]
fn armour_doubles_the_capacity_without_granting_immunity() {
    let mut scene = scene();
    assert!(!scene.react_player_to_hit(CombatantId::WORLD, 119, true, false, false, &ctx(100)));
    assert!(scene.react_player_to_hit(CombatantId::WORLD, 1, true, false, false, &ctx(101)));
}

#[test]
fn an_ordinary_hit_without_a_reaction_clip_does_not_interrupt_the_player() {
    let mut scene = scene();
    scene.anim_state = PlayerAnim::Run;
    assert!(!scene.react_player_to_hit(CombatantId::WORLD, 10, false, false, false, &ctx(100)));
    assert_eq!(scene.anim_state, PlayerAnim::Run);
    assert_eq!(scene.game_entities.actor_action().lock_until, SimTick::ZERO);
    assert_ne!(
        scene.game_entities.actor().poise,
        psx_game_runtime::poise::Poise::EMPTY,
        "the hit still counted toward posture"
    );
}

#[test]
fn a_shot_only_hit_never_interrupts_unless_it_breaks_posture() {
    let mut scene = scene();
    assert!(!scene.react_player_to_hit(CombatantId::WORLD, 30, false, true, false, &ctx(100)));
    assert_eq!(scene.anim_state, PlayerAnim::Idle);
    assert!(scene.react_player_to_hit(CombatantId::WORLD, 30, false, true, false, &ctx(101)));
    assert_eq!(
        scene.anim_state,
        PlayerAnim::Knockdown,
        "a break knocks her down"
    );
}

#[test]
fn a_stunned_player_accumulates_no_posture_and_cannot_be_broken_again() {
    let mut scene = scene();
    scene.anim_state = PlayerAnim::Stun;
    assert!(!scene.react_player_to_hit(CombatantId::WORLD, 1000, false, false, false, &ctx(100)));
    assert_eq!(
        scene.game_entities.actor().poise,
        psx_game_runtime::poise::Poise::EMPTY
    );
    assert_eq!(scene.game_entities.actor().flow.break_grace, 0);
}

#[test]
fn a_dying_player_ignores_further_hits() {
    let mut scene = scene();
    scene.hazard_death_ticks_remaining = 10;
    assert!(!scene.react_player_to_hit(CombatantId::WORLD, 1000, false, false, false, &ctx(100)));
    assert_eq!(
        scene.game_entities.actor().poise,
        psx_game_runtime::poise::Poise::EMPTY
    );
    assert_eq!(scene.anim_state, PlayerAnim::Idle);
}

#[test]
fn a_break_discards_queued_attack_and_dodge_input() {
    let mut scene = scene();
    scene
        .game_entities
        .actor_action_mut()
        .buffer
        .request(1, 100);
    scene.game_entities.actor_action_mut().evade_buffer = 8;
    scene.loco = LocoPhase::Cruise;
    assert!(scene.react_player_to_hit(CombatantId::WORLD, 60, false, false, false, &ctx(101)));
    assert_eq!(
        scene.game_entities.actor_action_mut().buffer.take(102),
        None
    );
    assert_eq!(scene.game_entities.actor_action().evade_buffer, 0);
    assert_eq!(scene.loco, LocoPhase::Idle);
}

#[test]
fn posture_recovers_after_a_quiet_interval_not_per_update_cadence() {
    let mut scene = scene();
    assert!(!scene.react_player_to_hit(CombatantId::WORLD, 30, false, false, false, &ctx(100)));
    scene
        .game_entities
        .actor_mut()
        .poise
        .tick(psx_game_runtime::poise::POISE_RESET_TICKS);
    assert_eq!(
        scene.game_entities.actor().poise,
        psx_game_runtime::poise::Poise::EMPTY
    );
    assert!(!scene.react_player_to_hit(CombatantId::WORLD, 30, false, false, false, &ctx(300)));
}

// ---------------------------------------------------------------------------
// Stance swap and its timing.
// ---------------------------------------------------------------------------

#[test]
fn a_swap_flips_the_stance_and_starts_the_default_cooldown() {
    let mut scene = scene();
    let config = scene.player_stance_config;
    assert_eq!(config.swap_cooldown_ticks, 30);
    assert!(scene.game_entities.actor_mut().stance.request_swap(&config));
    assert_eq!(
        scene.game_entities.actor_mut().stance.active(),
        VitalityChannelId::Two
    );
    assert_eq!(scene.game_entities.actor_mut().stance.swap_cooldown(), 30);
    assert_eq!(
        scene.game_entities.actor_mut().stance.swap_elapsed_ticks(),
        0
    );
    assert!(!scene.game_entities.actor().stance.can_swap());
}

#[test]
fn a_second_swap_is_refused_for_exactly_the_cooldown() {
    let mut scene = scene();
    let config = scene.player_stance_config;
    scene.game_entities.actor_mut().stance.request_swap(&config);
    for _ in 0..29 {
        scene.game_entities.actor_mut().stance.tick();
        assert!(!scene.game_entities.actor_mut().stance.request_swap(&config));
    }
    scene.game_entities.actor_mut().stance.tick();
    assert!(scene.game_entities.actor_mut().stance.can_swap());
    assert!(scene.game_entities.actor_mut().stance.request_swap(&config));
    assert_eq!(
        scene.game_entities.actor_mut().stance.active(),
        VitalityChannelId::One
    );
}

#[test]
fn a_hit_never_swaps_the_stance_or_touches_the_cooldown() {
    let mut scene = scene();
    let config = scene.player_stance_config;
    scene.game_entities.actor_mut().stance.request_swap(&config);
    for _ in 0..20 {
        scene.game_entities.actor_mut().stance.tick();
    }
    // Mid-cooldown, a big hit: there is no pool to break, so no forced swap.
    assert!(!scene.apply_untyped_player_damage(150));
    assert_eq!(
        scene.game_entities.actor().stance.active(),
        VitalityChannelId::Two
    );
    assert_eq!(scene.game_entities.actor().stance.swap_elapsed_ticks(), 20);
    assert_eq!(scene.game_entities.actor().stance.swap_cooldown(), 10);
}

#[test]
fn the_swap_cooldown_is_the_one_the_enemies_receive() {
    let mut scene = scene();
    scene.player_stance_config.swap_cooldown_ticks = 77;
    scene
        .game_entities
        .set_stance_swap_delay(scene.player_stance_config.swap_cooldown_ticks);
    let config = scene.player_stance_config;
    scene.game_entities.actor_mut().stance.request_swap(&config);
    assert_eq!(scene.game_entities.actor_mut().stance.swap_cooldown(), 77);
}

#[test]
fn nothing_regenerates_health_during_play() {
    // The old inactive pool recovered 10 here (90 ticks of delay, then a
    // quarter point a tick). The one bar has no recovery: a swap and the
    // passing ticks leave the damage where it is.
    let mut scene = scene();
    scene.apply_untyped_player_damage(20);
    let config = scene.player_stance_config;
    scene.game_entities.actor_mut().stance.request_swap(&config);
    for _ in 0..600 {
        scene.game_entities.actor_mut().advance_stance(1);
        scene.game_entities.actor_mut().tick(1, false, false);
    }
    assert_eq!(health(&scene), 180);
}

// ---------------------------------------------------------------------------
// Invulnerability windows.
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_player_is_vulnerable() {
    let scene = scene();
    assert_eq!(
        scene.game_entities.actor().stance.swap_elapsed_ticks(),
        u16::MAX
    );
    assert!(!scene.player_invulnerable(&ctx(100)));
}

#[test]
fn a_swap_is_invulnerable_for_the_roll_window_counting_the_press_tick() {
    let mut scene = scene();
    scene.character = Some(character(false));
    assert_eq!(scene.motor_config().roll_invulnerable_frames, TEST_I_FRAMES);
    let config = scene.player_stance_config;
    // The press and the first contact check share a tick: elapsed 0 at the
    // press, 1 when the first contact is resolved after `tick`.
    scene.game_entities.actor_mut().stance.request_swap(&config);
    let mut protected = 0u16;
    for _ in 0..40 {
        if scene.player_invulnerable(&ctx(100)) {
            protected += 1;
        } else {
            break;
        }
        scene.game_entities.actor_mut().stance.tick();
    }
    // elapsed 0..=TEST_I_FRAMES inclusive.
    assert_eq!(protected, u16::from(TEST_I_FRAMES) + 1);
    assert!(!scene.player_invulnerable(&ctx(100)));
}

#[test]
fn the_player_asks_the_combatant_rule_every_enemy_asks() {
    // The player's i-frames are `Combatant::invulnerable` over her own data
    // (the character's roll frames) and her action, the same call an enemy
    // defender makes with its record's count and no action protection.
    let mut scene = scene();
    scene.character = Some(character(false));
    let config = scene.player_stance_config;
    scene.game_entities.actor_mut().stance.request_swap(&config);
    for _ in 0..40 {
        let defender = scene.player_defender(&ctx(100));
        assert_eq!(defender.swap_invulnerable_ticks, TEST_I_FRAMES);
        assert_eq!(defender.action, ActionProtection::NONE, "idle: no action");
        assert_eq!(
            scene.player_invulnerable(&ctx(100)),
            scene
                .game_entities
                .actor()
                .invulnerable(TEST_I_FRAMES, ActionProtection::NONE)
        );
        scene.game_entities.actor_mut().stance.tick();
    }
}

#[test]
fn a_hit_grants_no_invulnerable_window() {
    // The old forced swap on an emptied pool handed out the swap i-frames;
    // with one bar a hit swaps nothing.
    let mut scene = scene();
    scene.apply_untyped_player_damage(150);
    assert!(!scene.player_invulnerable(&ctx(100)));
}

#[test]
fn a_roll_is_invulnerable_for_its_authored_frames_then_vulnerable() {
    use psx_engine::{CollisionTrace, CollisionTraceProvider, CollisionTraceQuery};
    struct OpenFloor;
    impl CollisionTraceProvider for OpenFloor {
        fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool {
            *output = CollisionTrace::unobstructed(query.end);
            true
        }
    }
    let mut scene = scene();
    scene.character = Some(character(false));
    let config = scene.motor_config();
    let evade = CharacterMotorInput {
        walk: 1,
        move_x: Q12::from_raw(0),
        move_z: Q12::from_raw(4096),
        evade: true,
        ..CharacterMotorInput::default()
    };
    scene
        .game_entities
        .actor_motor_mut()
        .update_vblanks_with_trace_provider(&mut OpenFloor, evade, config, 1)
        .unwrap();
    let mut protected = 0;
    while scene.player_invulnerable(&ctx(100)) && protected < 100 {
        protected += 1;
        scene
            .game_entities
            .actor_motor_mut()
            .update_vblanks_with_trace_provider(
                &mut OpenFloor,
                CharacterMotorInput::default(),
                config,
                1,
            )
            .unwrap();
    }
    // The update that starts the roll is itself the first protected frame,
    // so the polls between ticks see one fewer than the authored count.
    assert_eq!(protected + 1, i32::from(config.roll_invulnerable_frames));
}

#[test]
fn an_authored_invulnerable_window_replaces_the_motor_protection_and_can_close_it() {
    use psx_game_runtime::combat_timing::Sample;
    let action = CharacterAnimationAction::LightAttack.to_index() as u8;
    let windows = [CharacterCombatWindow {
        action,
        kind: CombatWindowKind::Invulnerable,
        start: 4,
        end: 9,
    }];
    // Phase is Q12 cooked frames; the window is half open.
    let at = |frame: u32| Sample::new(&windows, action, frame << 12);
    assert_eq!(at(3).active(CombatWindowKind::Invulnerable), Some(false));
    assert_eq!(at(4).active(CombatWindowKind::Invulnerable), Some(true));
    assert_eq!(at(8).active(CombatWindowKind::Invulnerable), Some(true));
    assert_eq!(at(9).active(CombatWindowKind::Invulnerable), Some(false));
    // Another action, or no window, inherits the legacy motor rule.
    assert_eq!(
        Sample::new(&windows, action + 1, 5 << 12).active(CombatWindowKind::Invulnerable),
        None
    );
    assert_eq!(Sample::LEGACY.active(CombatWindowKind::Invulnerable), None);
}

#[test]
fn a_player_without_authored_windows_samples_the_legacy_rule() {
    let scene = scene();
    let sample = scene.player_combat_sample(&ctx(100));
    assert_eq!(sample.active(CombatWindowKind::Invulnerable), None);
    assert_eq!(sample.active(CombatWindowKind::Armored), None);
}

// ---------------------------------------------------------------------------
// Energy through CombatFlow.
// ---------------------------------------------------------------------------

#[test]
fn energy_constants_are_the_prototype_economy() {
    assert_eq!(ENERGY_MAX, 100);
    assert_eq!(SHOT_COST, 20);
    assert_eq!(MELEE_GAIN, 12);
    assert_eq!(HEAVY_GAIN, 20);
}

#[test]
fn a_shot_costs_twenty_and_five_shots_empty_the_bar() {
    let mut scene = scene();
    for left in (0..5u16).rev() {
        assert!(scene
            .game_entities
            .actor()
            .flow
            .can_shoot(&CombatStats::PLAYER));
        assert!(scene
            .game_entities
            .actor_mut()
            .flow
            .spend_shot(&CombatStats::PLAYER));
        assert_eq!(
            scene.game_entities.actor_mut().flow.energy,
            left * SHOT_COST
        );
    }
    assert!(!scene
        .game_entities
        .actor_mut()
        .flow
        .can_shoot(&CombatStats::PLAYER));
    assert!(!scene
        .game_entities
        .actor_mut()
        .flow
        .spend_shot(&CombatStats::PLAYER));
    assert_eq!(scene.game_entities.actor_mut().flow.energy, 0);
}

#[test]
fn a_light_melee_hit_gains_twelve_and_a_heavy_hit_twenty_and_the_bar_caps() {
    let mut scene = scene();
    scene.game_entities.actor_mut().flow.energy = 0;
    scene.anim_state = PlayerAnim::LightAttack;
    scene.report_player_melee_stats(MeleeArcStats {
        hits: 1,
        staggers: 0,
        deaths: 0,
        parries: 0,
    });
    assert_eq!(scene.game_entities.actor().flow.energy, MELEE_GAIN);
    assert_eq!(scene.game_entities.actor().flow.followup, 0);
    scene.anim_state = PlayerAnim::HeavyAttack;
    scene.report_player_melee_stats(MeleeArcStats {
        hits: 1,
        staggers: 0,
        deaths: 0,
        parries: 0,
    });
    assert_eq!(
        scene.game_entities.actor().flow.energy,
        MELEE_GAIN + HEAVY_GAIN
    );
    assert_eq!(
        scene.game_entities.actor().flow.followup,
        1,
        "a heavy hit opens a follow-up"
    );
    scene.report_player_melee_stats(MeleeArcStats {
        hits: 10,
        staggers: 0,
        deaths: 0,
        parries: 0,
    });
    assert_eq!(scene.game_entities.actor().flow.energy, ENERGY_MAX);
}

#[test]
fn every_enemy_connected_by_one_swing_pays_energy_separately() {
    let mut scene = scene();
    scene.game_entities.actor_mut().flow.energy = 0;
    scene.anim_state = PlayerAnim::LightAttack;
    scene.report_player_melee_stats(MeleeArcStats {
        hits: 3,
        staggers: 0,
        deaths: 0,
        parries: 0,
    });
    assert_eq!(scene.game_entities.actor().flow.energy, 3 * MELEE_GAIN);
}

#[test]
fn a_connected_melee_hit_queues_a_weight_matched_hit_cue() {
    let mut scene = scene();
    scene.anim_state = PlayerAnim::LightAttack;
    scene.report_player_melee_stats(MeleeArcStats {
        hits: 1,
        staggers: 0,
        deaths: 0,
        parries: 0,
    });
    assert_eq!(
        scene.gameplay_sfx_events & LevelGameplaySfxEvent::LightHit.bit(),
        LevelGameplaySfxEvent::LightHit.bit()
    );
    scene.gameplay_sfx_events = 0;
    scene.anim_state = PlayerAnim::VertHeavyAttack;
    scene.report_player_melee_stats(MeleeArcStats {
        hits: 1,
        staggers: 0,
        deaths: 1,
        parries: 0,
    });
    assert_ne!(
        scene.gameplay_sfx_events & LevelGameplaySfxEvent::HeavyHit.bit(),
        0
    );
    assert_ne!(
        scene.gameplay_sfx_events & LevelGameplaySfxEvent::EnemyDeath.bit(),
        0
    );
}

#[test]
fn an_arch_refills_twenty_energy_and_health_a_second_and_stops_while_firing() {
    let mut scene = scene();
    scene.apply_untyped_player_damage(100);
    scene.game_entities.actor_mut().flow.energy = 0;
    let stats = scene.player_stats;
    // 20 per second at 60 ticks: one point every third tick.
    for _ in 0..60 {
        scene
            .game_entities
            .actor_mut()
            .air_tick(true, false, false, &stats);
    }
    assert_eq!(scene.game_entities.actor_mut().flow.energy, 20);
    assert_eq!(health(&scene), 120, "health comes back on the same beat");
    for _ in 0..60 {
        scene
            .game_entities
            .actor_mut()
            .air_tick(true, false, true, &stats);
    }
    assert_eq!(
        scene.game_entities.actor_mut().flow.energy,
        20,
        "firing pauses the recharge"
    );
    assert_eq!(health(&scene), 120);
    assert_eq!(
        scene.game_entities.actor().flow.air_left,
        AIR_TICKS - 120,
        "but the allowance still drains"
    );
}

#[test]
fn the_arch_allowance_runs_out_after_its_ticks_and_needs_ground_time_to_rearm() {
    let mut flow = CombatFlow::FULL;
    let mut expired = false;
    for _ in 0..AIR_TICKS {
        expired = flow
            .air_tick(true, false, false, &CombatStats::PLAYER)
            .exhausted;
    }
    assert!(expired);
    assert_eq!(flow.air_left, 0);
    // Touching ground briefly does not refresh it.
    for _ in 0..119 {
        flow.air_tick(false, true, false, &CombatStats::PLAYER);
    }
    assert_eq!(flow.air_left, 0);
    flow.air_tick(false, true, false, &CombatStats::PLAYER);
    assert_eq!(flow.air_left, AIR_TICKS);
}

#[test]
fn taking_damage_does_not_change_the_energy_bar() {
    let mut scene = scene();
    scene.game_entities.actor_mut().flow.energy = 40;
    scene.apply_untyped_player_damage(20);
    scene.apply_typed_player_damage(ProjectileDamageChannel::Zenith, 20);
    assert_eq!(scene.game_entities.actor_mut().flow.energy, 40);
}

#[test]
fn a_shot_is_free_to_aim_but_the_energy_gate_follows_the_stance_for_the_player() {
    // The player's gate is energy plus the Zenith stance; the energy leaf is
    // the same `CombatFlow` the enemies carry.
    let flow = CombatFlow::FULL;
    assert!(flow.can_shoot(&CombatStats::PLAYER));
    let mut empty = flow;
    empty.energy = SHOT_COST - 1;
    assert!(!empty.can_shoot(&CombatStats::PLAYER));
}

#[test]
fn the_energy_economy_and_health_are_the_characters_data() {
    // Step 4: the numbers come off the character's CombatStats, not
    // constants, so a character can carry its own.
    let mut scene = scene();
    scene.player_stats = CombatStats {
        max_health: 100,
        energy_max: 60,
        shot_cost: 30,
        light_gain: 5,
        ..CombatStats::PLAYER
    };
    scene.reset_life_combat();
    assert_eq!(health(&scene), 100);
    assert_eq!(scene.game_entities.actor().flow.energy, 60);
    assert!(scene
        .game_entities
        .actor_mut()
        .flow
        .spend_shot(&scene.player_stats));
    assert!(scene
        .game_entities
        .actor_mut()
        .flow
        .spend_shot(&scene.player_stats));
    assert!(!scene
        .game_entities
        .actor_mut()
        .flow
        .can_shoot(&scene.player_stats));
    scene.anim_state = PlayerAnim::LightAttack;
    scene.report_player_melee_stats(MeleeArcStats {
        hits: 1,
        staggers: 0,
        deaths: 0,
        parries: 0,
    });
    assert_eq!(scene.game_entities.actor().flow.energy, 5);
}

// ---------------------------------------------------------------------------
// Death and respawn.
// ---------------------------------------------------------------------------

#[test]
fn the_death_countdown_is_thirty_ticks() {
    assert_eq!(HAZARD_DEATH_TICKS, 30);
}

#[test]
fn arming_death_locks_the_death_clip_and_drops_targeting() {
    let mut scene = scene();
    scene.lock_target = Some(1);
    scene.soft_lock_target = Some(1);
    scene.camera_recenter_requested = true;
    scene.arm_player_death(
        true,
        HAZARD_DEATH_TICKS,
        SimTick::from_u32(100),
        VideoHz::NTSC,
    );
    assert_eq!(scene.hazard_death_ticks_remaining, HAZARD_DEATH_TICKS);
    assert!(scene.death_by_combat);
    assert_eq!(scene.anim_state, PlayerAnim::Death);
    assert_eq!(
        scene.game_entities.actor_action().lock_until,
        SimTick::from_u32(130)
    );
    assert!(scene.lock_target.is_none() && scene.soft_lock_target.is_none());
    assert!(!scene.camera_recenter_requested);
}

#[test]
fn a_hazard_death_is_not_flagged_as_combat() {
    let mut scene = scene();
    scene.arm_player_death(
        false,
        HAZARD_DEATH_TICKS,
        SimTick::from_u32(5),
        VideoHz::NTSC,
    );
    assert!(!scene.death_by_combat);
}

#[test]
fn a_zero_delay_still_arms_a_one_tick_countdown() {
    let mut scene = scene();
    scene.arm_player_death(true, 0, SimTick::from_u32(5), VideoHz::NTSC);
    assert_eq!(scene.hazard_death_ticks_remaining, 1);
}

#[test]
fn a_new_combat_life_restores_every_player_combat_resource() {
    let mut scene = scene();
    scene.character = Some(character(true));
    assert!(scene.apply_untyped_player_damage(1000));
    assert!(scene.game_entities.actor().defeated());
    scene.game_entities.actor_mut().flow.energy = 0;
    scene.game_entities.actor_mut().flow.broke();
    scene.game_entities.actor_mut().poise.hit(30, 60, false);
    scene
        .game_entities
        .actor_action_mut()
        .buffer
        .request(1, 100);
    scene.game_entities.actor_action_mut().evade_buffer = 8;
    scene.swing_hit_mask = 5;
    scene
        .combat_projectiles
        .spawn(bolt(ProjectileDamageChannel::Horizon, 20, 40))
        .unwrap();
    scene.reset_life_combat();
    assert_eq!(health(&scene), PLAYER_MAX_HEALTH);
    assert_eq!(
        scene.game_entities.actor().stance.active(),
        VitalityChannelId::One
    );
    assert_eq!(
        scene.game_entities.actor().stance.swap_elapsed_ticks(),
        u16::MAX
    );
    assert_eq!(scene.game_entities.actor().flow, CombatFlow::FULL);
    assert_eq!(
        scene.game_entities.actor().poise,
        psx_game_runtime::poise::Poise::EMPTY
    );
    assert_eq!(
        scene.game_entities.actor_action_mut().buffer.take(101),
        None
    );
    assert_eq!(scene.game_entities.actor_action().evade_buffer, 0);
    assert_eq!(scene.swing_hit_mask, 0);
    assert_eq!(scene.combat_projectiles.len(), 0);
}

#[test]
fn respawn_after_a_combat_death_restores_the_life_and_clears_the_countdown() {
    let mut scene = scene();
    scene.apply_untyped_player_damage(1000);
    scene.game_entities.actor_mut().flow.energy = 7;
    scene.arm_player_death(
        true,
        HAZARD_DEATH_TICKS,
        SimTick::from_u32(100),
        VideoHz::NTSC,
    );
    scene.respawn_after_death();
    assert_eq!(scene.hazard_death_ticks_remaining, 0);
    assert!(!scene.death_by_combat);
    assert_eq!(scene.anim_state, PlayerAnim::Idle);
    assert_eq!(scene.game_entities.actor_action().lock_until, SimTick::ZERO);
    assert_eq!(health(&scene), PLAYER_MAX_HEALTH);
    assert_eq!(scene.game_entities.actor().flow.energy, ENERGY_MAX);
}
