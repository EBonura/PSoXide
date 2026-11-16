//! The controller seam: the pad and the AI speak one [`CombatIntent`], and
//! the rules run whatever a controller asks through the same code.

use super::combat_baseline_tests::ctx;
use super::*;
use psx_engine::{AnalogSticks, PadMode, PadState};

/// A context whose pad holds `held` this tick and held `before` last tick,
/// with the left stick at `stick` (centred pad units, x right, y up).
fn pad_ctx(before: u16, held: u16, stick: (i16, i16)) -> Ctx {
    let mut ctx = ctx(100);
    let mut pad = PadState::NONE;
    pad.mode = PadMode::Analog;
    pad.id_low = 0x73;
    pad.sticks = AnalogSticks {
        left_x: (128 + stick.0) as u8,
        left_y: (128 - stick.1) as u8,
        right_x: 128,
        right_y: 128,
    };
    ctx.pad_prev = pad;
    ctx.pad_prev.buttons = psx_pad::ButtonState::from_bits(before);
    pad.buttons = psx_pad::ButtonState::from_bits(held);
    ctx.pad = pad;
    ctx
}

#[test]
fn each_combat_button_is_its_verb() {
    let cases = [
        (button::TRIANGLE, CombatActions::of(CombatAction::Swap)),
        (button::R1, CombatActions::of(CombatAction::LightAttack)),
        (
            button::R2,
            CombatActions::of(CombatAction::HeavyAttack).with(CombatAction::Shoot),
        ),
        (button::L2, CombatActions::of(CombatAction::Aim)),
        (button::CIRCLE, CombatActions::of(CombatAction::Evade)),
        (button::R3, CombatActions::of(CombatAction::Lock)),
        // Not combat: interaction, menus and the camera stay on the pad.
        (button::CROSS, CombatActions::NONE),
        (button::SELECT, CombatActions::NONE),
        (button::START, CombatActions::NONE),
    ];
    for (bit, actions) in cases {
        let press = pad_intent(&pad_ctx(0, bit, (0, 0)), 0);
        assert_eq!(press.pressed, actions, "press {bit:#06x}");
        assert_eq!(press.held, actions, "press {bit:#06x}");
        assert_eq!(press.released, CombatActions::NONE);
        let hold = pad_intent(&pad_ctx(bit, bit, (0, 0)), 0);
        assert_eq!(hold.pressed, CombatActions::NONE, "hold {bit:#06x}");
        assert_eq!(hold.held, actions, "hold {bit:#06x}");
        let release = pad_intent(&pad_ctx(bit, 0, (0, 0)), 0);
        assert_eq!(release.held, CombatActions::NONE, "release {bit:#06x}");
        assert_eq!(release.released, actions, "release {bit:#06x}");
    }
}

#[test]
fn the_stick_is_the_move_and_a_centred_stick_stands_still() {
    let still = pad_intent(&pad_ctx(0, 0, (0, 0)), 8);
    assert_eq!(still.movement, CombatMove::Still);
    assert_eq!(still, CombatIntent::IDLE);
    // Inside the deadzone is still too.
    assert_eq!(
        pad_intent(&pad_ctx(0, 0, (5, -5)), 8).movement,
        CombatMove::Still
    );
    let forward = pad_intent(&pad_ctx(0, 0, (0, 100)), 8);
    let CombatMove::Stick { x, z } = forward.movement else {
        panic!("stick expected, got {:?}", forward.movement);
    };
    assert_eq!(x, 0);
    assert!(z > 0);
    // The motor reads the very vector the stick gave.
    assert_eq!(
        intent_stick(&forward).raw(),
        local_move_axes(&pad_ctx(0, 0, (0, 100)), 8).raw()
    );
    // The D-pad is a full stick when the analog stick rests.
    let right = pad_intent(&pad_ctx(0, button::RIGHT, (0, 0)), 8);
    assert_eq!(right.movement, CombatMove::Stick { x: STICK_MAX, z: 0 });
    assert!(right.pressed.is_empty() && right.held.is_empty());
}

#[test]
fn several_presses_in_one_tick_all_arrive() {
    let both = pad_intent(&pad_ctx(0, button::TRIANGLE | button::R1, (0, 0)), 0);
    assert!(both.pressed(CombatAction::Swap) && both.pressed(CombatAction::LightAttack));
    assert_eq!(both.attack(), Some(CombatAction::LightAttack));
}

/// An engaged enemy with both moves (it may swap and strike), alone in its
/// own entity table.
fn hybrid_fighter() -> (
    &'static [LevelGameEntityRecord],
    std::boxed::Box<RuntimeGameEntities>,
) {
    let records: &'static [LevelGameEntityRecord] =
        std::boxed::Box::leak(std::boxed::Box::new([LevelGameEntityRecord {
            flags: psx_level::game_entity_flags::ENABLED
                | psx_level::game_entity_flags::RANGED_ATTACK
                | psx_level::game_entity_flags::MELEE_ATTACK
                | psx_level::game_entity_flags::LIGHT_ATTACK
                | psx_level::game_entity_flags::HEAVY_ATTACK,
            ..GAME_ENTITIES[0]
        }]));
    let mut entities = std::boxed::Box::new(RuntimeGameEntities::EMPTY);
    entities.spawn_from_records(records);
    entities.set_stance_swap_delay(30);
    entities.set_spatial_active_mask(u64::MAX);
    (records, entities)
}

#[test]
fn an_intent_runs_the_same_whether_the_pad_or_the_ai_sent_it() {
    use psx_game_runtime::entities::{GameEntityState, GameEntityTickStats, NoClipMover};
    for (bit, action) in [
        (button::TRIANGLE, CombatAction::Swap),
        (button::R1, CombatAction::LightAttack),
        (button::R2, CombatAction::HeavyAttack),
    ] {
        let from_pad = pad_intent(&pad_ctx(0, bit, (0, 0)), 0);
        let from_ai = CombatIntent {
            pressed: CombatActions::of(action),
            ..CombatIntent::IDLE
        };
        let (records, mut pad_driven) = hybrid_fighter();
        let (_, mut ai_driven) = hybrid_fighter();
        let stance_before = pad_driven.stance(0);
        let mut stats = GameEntityTickStats::default();
        pad_driven.apply_intent(&records[0], 0, from_pad, &mut NoClipMover, 2, &mut stats);
        ai_driven.apply_intent(&records[0], 0, from_ai, &mut NoClipMover, 2, &mut stats);
        let outcome = |e: &RuntimeGameEntities| {
            (
                e.stance(0),
                e.state(0),
                e.attack_kind(0),
                e.position(0),
                e.stance_swap_cooldown(0),
            )
        };
        assert_eq!(outcome(&pad_driven), outcome(&ai_driven), "{action:?}");
        match action {
            CombatAction::Swap => assert_ne!(pad_driven.stance(0), stance_before),
            _ => assert_eq!(pad_driven.state(0), GameEntityState::Windup),
        }
    }
}
