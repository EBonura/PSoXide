//! Player movement and camera input helpers.

use psx_engine::{camera_relative_move_axes, Angle, InputAxis, InputAxisProfile, InputVector};

use super::*;

/// The pad as a combat controller: the one adapter from buttons and the
/// left stick to the player's [`CombatIntent`]. Real input and the duel's
/// synthetic pad both come through here, and the player's combat rules read
/// only the intent.
///
/// Triangle swaps, R1 strikes light, R2 strikes heavy or fires (one trigger:
/// the stance picks which verb the rules perform), L2 aims, Circle evades
/// (a tap; a hold runs) and R3 toggles the lock. The stick is the move, in
/// the pad's own frame ([`CombatMove::Stick`]). The camera stick, menus and
/// interaction stay on the pad: they are not combat verbs.
// One small out-of-line call per tick keeps the large gameplay update from
// growing (and from changing how much else gets inlined into it).
#[inline(never)]
pub(crate) fn pad_intent(ctx: &Ctx, deadzone: i16) -> CombatIntent {
    let held = ctx.pad.buttons.bits();
    let before = ctx.pad_prev.buttons.bits();
    let (x, z) = local_move_axes(ctx, deadzone).raw();
    CombatIntent {
        movement: if x == 0 && z == 0 {
            CombatMove::Still
        } else {
            CombatMove::Stick { x, z }
        },
        face: None,
        aim: None,
        pressed: pad_actions(held & !before),
        held: pad_actions(held),
        released: pad_actions(before & !held),
    }
}

/// The combat actions a mask of pad buttons asks for.
fn pad_actions(buttons: u16) -> CombatActions {
    let on = |mask: u16| buttons & mask != 0;
    CombatActions::NONE
        .with_if(CombatAction::Swap, on(button::TRIANGLE))
        .with_if(CombatAction::LightAttack, on(ACTIVE_LIGHT_ATTACK_BUTTON))
        .with_if(CombatAction::HeavyAttack, on(ACTIVE_HEAVY_ATTACK_BUTTON))
        .with_if(CombatAction::Shoot, on(ACTIVE_HEAVY_ATTACK_BUTTON))
        .with_if(CombatAction::Aim, on(button::L2))
        .with_if(CombatAction::Evade, on(EVADE_RUN_BUTTON))
        .with_if(CombatAction::Lock, on(button::R3))
}

/// The stick of a pad intent, as the motor's local input vector.
pub(crate) fn intent_stick(intent: &CombatIntent) -> InputVector {
    match intent.movement {
        CombatMove::Stick { x, z } => InputVector::from_centered(x, z),
        _ => InputVector::ZERO,
    }
}

pub(crate) fn motor_input(
    intent: &CombatIntent,
    camera_yaw: Angle,
    deadzone: i16,
    sprint: bool,
    evade: bool,
    facing_yaw: Option<Angle>,
) -> CharacterMotorInput {
    let movement =
        camera_relative_move_axes(intent_stick(intent), camera_yaw, axis_profile(deadzone));

    CharacterMotorInput {
        turn: 0,
        walk: movement.forward,
        move_x: movement.x,
        move_z: movement.z,
        facing_yaw,
        sprint,
        evade,
    }
}

pub(crate) fn local_move_axes(ctx: &Ctx, deadzone: i16) -> InputVector {
    let (left_x, left_y) = ctx.pad.sticks.left_centered();
    let left = InputVector::from_centered(left_x, left_y);
    let stick_mag = isqrt_i32(left.magnitude_squared());
    if stick_mag > deadzone as i32 {
        return InputVector::new(left.x, left.y.inverted()).clamped(STICK_MAX);
    }

    let mut strafe = 0i16;
    let mut forward = 0i16;
    if ctx.is_held(button::RIGHT) {
        strafe += STICK_MAX;
    }
    if ctx.is_held(button::LEFT) {
        strafe -= STICK_MAX;
    }
    if ctx.is_held(button::UP) {
        forward += STICK_MAX;
    }
    if ctx.is_held(button::DOWN) {
        forward -= STICK_MAX;
    }
    InputVector::from_centered(strafe, forward)
}

pub(crate) fn player_anim_from_motor(anim: CharacterMotorAnim) -> PlayerAnim {
    match anim {
        CharacterMotorAnim::Idle => PlayerAnim::Idle,
        CharacterMotorAnim::Walk => PlayerAnim::Walk,
        CharacterMotorAnim::WalkBackward => PlayerAnim::WalkBackward,
        CharacterMotorAnim::StrafeLeft => PlayerAnim::StrafeLeft,
        CharacterMotorAnim::StrafeRight => PlayerAnim::StrafeRight,
        CharacterMotorAnim::Run => PlayerAnim::Run,
        CharacterMotorAnim::Roll => PlayerAnim::Roll,
        CharacterMotorAnim::Quickstep => PlayerAnim::Quickstep,
        CharacterMotorAnim::DashLeft => PlayerAnim::DashLeft,
        CharacterMotorAnim::DashRight => PlayerAnim::DashRight,
    }
}

pub(crate) fn camera_input(
    ctx: &Ctx,
    orbit_speed_level: u8,
    deadzone: i16,
    accelerated: bool,
) -> ThirdPersonCameraInput {
    let (right_x, right_y) = camera_stick_axes(ctx, deadzone);
    if accelerated {
        let step = psx_engine::accelerated_orbit_step_q12(orbit_speed_level, false);
        return ThirdPersonCameraInput {
            yaw_delta_q12: stick_axis_delta(InputAxis::new(right_x.saturating_neg()), step, 0),
            pitch_delta_q12: stick_axis_delta(InputAxis::new(right_y), step, 0),
            recenter: false,
        };
    }
    ThirdPersonCameraInput {
        yaw_delta_q12: stick_to_yaw_delta(
            InputAxis::new(right_x.saturating_neg()),
            orbit_speed_level,
            0,
        ),
        pitch_delta_q12: stick_to_pitch_delta(InputAxis::new(right_y), orbit_speed_level, 0),
        // All four shoulder buttons belong to combat. Manual right-stick
        // orbit remains available; R3 requests recenter in the gameplay loop
        // when there is no eligible lock target.
        recenter: false,
    }
}

/// Apply the SDK's shared scaled-radial deadzone to the camera stick. The
/// returned axes already start at zero just outside the dead region, so camera
/// response helpers use an axis threshold of zero rather than applying the
/// setting a second time.
pub(crate) fn camera_stick_axes(ctx: &Ctx, deadzone: i16) -> (i16, i16) {
    let (right_x, right_y) = ctx.pad.sticks.right_centered();
    psx_engine::Deadzone::new(deadzone)
        .scaled(right_x, right_y)
        .unwrap_or((0, 0))
}

pub(crate) fn stick_to_yaw_delta(axis: InputAxis, orbit_speed_level: u8, deadzone: i16) -> i16 {
    stick_axis_delta(
        axis,
        scaled_camera_step(CAMERA_STICK_YAW_STEP, orbit_speed_level),
        deadzone,
    )
}

pub(crate) fn stick_to_pitch_delta(axis: InputAxis, orbit_speed_level: u8, deadzone: i16) -> i16 {
    stick_axis_delta(
        axis,
        scaled_camera_step(CAMERA_STICK_PITCH_STEP, orbit_speed_level),
        deadzone,
    )
}

pub(crate) fn stick_axis_delta(axis: InputAxis, max_step: i16, deadzone: i16) -> i16 {
    axis.scaled_step(axis_profile(deadzone), max_step)
}

pub(crate) fn scaled_camera_step(base: i16, orbit_speed_level: u8) -> i16 {
    let level =
        orbit_speed_level.clamp(MIN_CAMERA_ORBIT_SPEED_LEVEL, MAX_CAMERA_ORBIT_SPEED_LEVEL) as i32;
    clamp_i16((base as i32).saturating_mul(level) / DEFAULT_CAMERA_ORBIT_SPEED_LEVEL as i32)
}

pub(crate) fn scale_i16_by_vblanks(value: i16, delta_vblanks: u16) -> i16 {
    let scaled = (value as i32).saturating_mul(delta_vblanks.max(1) as i32);
    clamp_i16(scaled)
}

fn axis_profile(deadzone: i16) -> InputAxisProfile {
    InputAxisProfile::new(deadzone, STICK_MAX)
}

pub(crate) use psx_math::int32::{abs_i16, abs_i32, clamp_i16, isqrt_i32, square_i32_saturating};
