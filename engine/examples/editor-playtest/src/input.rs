//! Player movement and camera input helpers.

use psx_engine::{camera_relative_move_axes, Angle, InputAxis, InputAxisProfile, InputVector};

use super::*;

pub(crate) fn motor_input(
    ctx: &Ctx,
    camera_yaw: Angle,
    deadzone: i16,
    sprint: bool,
    evade: bool,
    facing_yaw: Option<Angle>,
) -> CharacterMotorInput {
    let movement = camera_relative_move_axes(
        local_move_axes(ctx, deadzone),
        camera_yaw,
        axis_profile(deadzone),
    );

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
) -> ThirdPersonCameraInput {
    let (right_x, right_y) = camera_stick_axes(ctx, deadzone);
    ThirdPersonCameraInput {
        yaw_delta_q12: stick_to_yaw_delta(
            InputAxis::new(right_x.saturating_neg()),
            orbit_speed_level,
            0,
        ),
        pitch_delta_q12: stick_to_pitch_delta(InputAxis::new(right_y), orbit_speed_level, 0),
        // All four shoulder buttons belong to combat. Manual right-stick
        // orbit remains available; camera recenter has no dedicated binding.
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

pub(crate) fn stick_to_radius_delta(axis: InputAxis, deadzone: i16) -> i32 {
    stick_axis_delta(axis, CAMERA_RADIUS_STEP as i16, deadzone) as i32
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

pub(crate) fn scale_i32_by_vblanks(value: i32, delta_vblanks: u16) -> i32 {
    value.saturating_mul(delta_vblanks.max(1) as i32)
}

fn axis_profile(deadzone: i16) -> InputAxisProfile {
    InputAxisProfile::new(deadzone, STICK_MAX)
}

pub(crate) use psx_math::int32::{abs_i16, abs_i32, clamp_i16, isqrt_i32, square_i32_saturating};
