//! Integer adaptation of the standard native lock composition.
use super::{signed_q12_angle, RoomPoint, ThirdPersonCameraConfig};
use psx_math::int32::isqrt_i32;

/// The reference orbit distance is a radius. Recover its pitch from the
/// authored vertical offset so lock-on does not inherit an extra height bias.
pub(super) fn pitch_from_height_offset(vertical: i32, distance: i32) -> i16 {
    let radius = distance.max(1);
    let divisor = radius / 16_383 + 1;
    let radius = (radius / divisor).max(1);
    let vertical = (vertical / divisor).clamp(-radius, radius);
    let horizontal = isqrt_i32(radius * radius - vertical * vertical);
    elevation_angle(vertical, horizontal)
}

/// Aim above the player, orbit toward the supplied enemy anchor, and place the
/// enemy above the screen center by 45% of the vertical half-FOV. The correction
/// accounts for the camera being behind the player rather than at the pivot.
/// Native follow/fulcrum stages are approximated by Cortex's current boom.
pub(super) fn lock_pitch_goal(
    focus: RoomPoint,
    target: RoomPoint,
    boom: i32,
    config: ThirdPersonCameraConfig,
) -> Option<i16> {
    // Bound components before squaring, preserving their direction by a shared
    // divisor. Even extreme room coordinates cannot overflow 32-bit arithmetic.
    let dx = target.x.saturating_sub(focus.x);
    let dy = target.y.saturating_sub(focus.y);
    let dz = target.z.saturating_sub(focus.z);
    let extent = dx
        .saturating_abs()
        .max(dy.saturating_abs())
        .max(dz.saturating_abs());
    if extent == 0 {
        return None;
    }
    let divisor = extent / 16_383 + 1;
    let (x, y, z) = (dx / divisor, dy / divisor, dz / divisor);
    let horizontal = isqrt_i32(x * x + z * z);
    let range = isqrt_i32(x * x + y * y + z * z).max(1);
    let elevation = i32::from(elevation_angle(y, horizontal));
    let shift = i32::from(config.fov_y_degrees.clamp(38, 48)) * 4096 * 45 / 72_000;
    let correction_sin =
        ((boom.clamp(0, 65_535) / divisor) * signed_q12_angle(shift as i16).sin().raw() / range)
            .clamp(0, 4096);
    let correction_cos = isqrt_i32(4096 * 4096 - correction_sin * correction_sin);
    let correction = i32::from(elevation_angle(correction_sin, correction_cos));

    // Native height thresholds .5..2 are normalized by the 1.42 focus offset.
    // The upper pitch limit blends from 40 to 70 degrees over this interval.
    let reference_height = config.target_height.clamp(1, 65_535);
    let low = (reference_height * 50 / 142).max(1);
    let high = (reference_height * 200 / 142).max(low + 1);
    let height_blend = (dy.saturating_abs() - low).clamp(0, high - low);
    let maximum = 455 + height_blend * (796 - 455) / (high - low);
    let pitch = (-elevation + shift + correction).clamp(-455, maximum);
    Some(pitch.clamp(
        i32::from(config.pitch_min_q12),
        i32::from(config.pitch_max_q12),
    ) as i16)
}

// The general movement atan approximation has about a degree of error at this
// framing angle. A bounded first-quadrant search uses the engine's trig table.
fn elevation_angle(vertical: i32, horizontal: i32) -> i16 {
    let mut low = 0i16;
    let mut high = 1024i16;
    let magnitude = vertical.saturating_abs();
    while low < high {
        let middle = (low + high) / 2;
        let angle = signed_q12_angle(middle);
        if angle.sin().raw() * horizontal < angle.cos().raw() * magnitude {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    if vertical < 0 {
        -low
    } else {
        low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ThirdPersonCameraConfig {
        let mut c = ThirdPersonCameraConfig::character(158, 61, 61);
        c.fov_y_degrees = 43;
        c.pitch_min_q12 = -455;
        c.pitch_max_q12 = 796;
        c
    }

    #[test]
    fn level_target_has_half_fov_shift_and_camera_distance_correction() {
        let c = config();
        let focus = RoomPoint::new(0, 61, 0);
        let pitch = lock_pitch_goal(focus, RoomPoint::new(0, 61, 400), 158, c).unwrap();
        // 9.675 degrees + asin(158*sin(9.675)/400) = 13.481 degrees.
        assert!((151..=155).contains(&pitch), "{pitch}");
        let compressed = lock_pitch_goal(focus, RoomPoint::new(0, 61, 400), 40, c).unwrap();
        assert!(compressed < pitch);
    }

    #[test]
    fn elevation_and_singular_targets_are_bounded() {
        let c = config();
        let focus = RoomPoint::new(0, 61, 0);
        assert_eq!(lock_pitch_goal(focus, focus, 158, c), None);
        let above = lock_pitch_goal(focus, RoomPoint::new(0, 206, 896), 158, c).unwrap();
        let below = lock_pitch_goal(focus, RoomPoint::new(0, -50, 896), 158, c).unwrap();
        assert!(above < below);
        for target in [
            RoomPoint::new(i32::MAX, i32::MIN, i32::MAX),
            RoomPoint::new(0, -10, 0),
        ] {
            let pitch = lock_pitch_goal(focus, target, 158, c).unwrap();
            assert!((-455..=796).contains(&pitch));
        }
        // Symmetric horizontal bearings have the same elevation framing.
        assert_eq!(
            lock_pitch_goal(focus, RoomPoint::new(400, 61, 0), 158, c),
            lock_pitch_goal(focus, RoomPoint::new(0, 61, -400), 158, c)
        );
    }
}
