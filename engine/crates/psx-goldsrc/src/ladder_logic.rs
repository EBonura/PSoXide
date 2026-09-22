//! Small, allocation-free ladder contact correction.
//!
//! GoldSrc never snaps the player through or away from the ladder plane: the
//! contacted origin is preserved and `PM_LadderMove` changes velocity only.
//! The PS1's whole-unit movement can first touch a thin ladder volume just past
//! a tangential corner, so the sole mount correction clamps that tangential
//! coordinate back inside the authored width.

const PLAYER_HALF_WIDTH: i32 = 16;
const PLAYER_HALF_HEIGHT: i32 = 36;
const DESCEND_PITCH: i16 = -300;

#[inline]
pub fn wants_descend(pitch: i16) -> bool {
    pitch < DESCEND_PITCH
}

/// AABB equivalent of GoldSrc `PM_Ladder`: `PM_HullForBsp` expands the ladder
/// model by the active player hull, then `PM_HullPointContents` tests the
/// current origin. There is no forward probe or special airborne branch.
#[inline]
pub fn touches(pos: [i32; 3], center: [i32; 3], half: [i32; 3]) -> bool {
    (pos[0] - center[0]).abs() <= half[0] + PLAYER_HALF_WIDTH
        // PM_HullPointContents classifies the axial exit plane as outside the
        // ladder hull. Cooked odd-height bounds round both their midpoint and
        // half-extent, which can otherwise add one unit to the reconstructed
        // top (c1a1's -148..805 ladder becomes center 329, half 477). Remove
        // that quantization unit from the vertical reach: without it the world
        // floor blocks the next climb step while ladder movement keeps gravity
        // disabled, so a normal top-out can never return to walking physics.
        && (pos[1] - center[1]).abs() < half[1] + PLAYER_HALF_HEIGHT - 1
        && (pos[2] - center[2]).abs() <= half[2] + PLAYER_HALF_WIDTH
}

/// Two-unit PS1 quantization assist for an intentional mount from the lower
/// half of a ladder.  Widen only the tangential edge: widening the thin/normal
/// axis grabs the ladder before the real player hull reaches its face.  That
/// skipped GoldSrc's final wall-contact frame on both c1a1d ladders and changed
/// the second ladder's exit momentum enough to miss the hanging crates.
///
/// Keep this separate from ordinary contact: applying the wider box at the
/// upper landing captures floor sidesteps as climb input.
#[inline]
pub fn touches_mount_assist(
    pos: [i32; 3],
    center: [i32; 3],
    half: [i32; 3],
    allow_upper: bool,
) -> bool {
    if !allow_upper && pos[1] > center[1] {
        return false;
    }
    let dx = (pos[0] - center[0]).abs();
    let dz = (pos[2] - center[2]).abs();
    let vertical = (pos[1] - center[1]).abs() <= half[1] + PLAYER_HALF_HEIGHT;
    if half[0] <= half[2] {
        vertical && dx <= half[0] + PLAYER_HALF_WIDTH && dz <= half[2] + PLAYER_HALF_WIDTH + 2
    } else {
        vertical && dx <= half[0] + PLAYER_HALF_WIDTH + 2 && dz <= half[2] + PLAYER_HALF_WIDTH
    }
}

/// Keep an already-mounted player attached across the same two units of
/// horizontal integer quantization used by the entry assist. Unlike the entry
/// helper this is valid over the full ladder height: the player has already
/// made an intentional mount, so an upper-half tolerance cannot accidentally
/// grab somebody walking across the landing. The thin/normal axis is never
/// widened, and the strict vertical exit plane still releases a normal topout.
#[inline(always)] // a plain hint let LTO outline it from HL's player tick
pub fn touches_attached(pos: [i32; 3], center: [i32; 3], half: [i32; 3]) -> bool {
    let dx = (pos[0] - center[0]).abs();
    let dz = (pos[2] - center[2]).abs();
    let vertical = (pos[1] - center[1]).abs() < half[1] + PLAYER_HALF_HEIGHT - 1;
    if half[0] <= half[2] {
        vertical && dx <= half[0] + PLAYER_HALF_WIDTH && dz <= half[2] + PLAYER_HALF_WIDTH + 2
    } else {
        vertical && dx <= half[0] + PLAYER_HALF_WIDTH + 2 && dz <= half[2] + PLAYER_HALF_WIDTH
    }
}

#[inline]
fn clamp_inside(value: i32, center: i32, half: i32) -> i32 {
    let inset = (half > 0) as i32;
    value.clamp(center - half + inset, center + half - inset)
}

/// Scale one signed semantic axis without the toward-zero bias that turns a
/// full +127 input into 9 when the authored speed is 10 units/tick.
#[inline]
pub fn axis_speed_nearest(axis: i32, speed: i32) -> i32 {
    let product = axis * speed;
    if product >= 0 {
        (product + 64) / 128
    } else {
        -((-product + 64) / 128)
    }
}

/// Q12 multiply with symmetric nearest rounding. Ladder movement often sits
/// on cardinal angles whose LUT values are one unit shy of 4096; truncating
/// those values turns an authored 10-unit sidestep into 9 every tick.
#[inline]
pub fn mul_q12_nearest(a: i32, b: i32) -> i32 {
    let product = a * b;
    if product >= 0 {
        (product + 2048) >> 12
    } else {
        -((-product + 2048) >> 12)
    }
}

/// Fixed-point form of GoldSrc's `PM_LadderMove` velocity decomposition.
///
/// `forward` and `right` are Q12 view vectors, `normal_axis` is the ladder's
/// thin horizontal axis (0 = X, 2 = Z), and `normal_sign` points from the
/// ladder towards the player. The result is Q6 world units per 20 Hz tick.
/// GoldSrc treats ladder input as buttons, so any non-zero semantic axis uses
/// the full authored speed; crouching supplies the smaller speed at the call
/// site.
#[inline]
pub fn goldsrc_velocity_q6(
    forward: [i32; 3],
    right: [i32; 3],
    fwd: i32,
    strafe: i32,
    speed_q6: i32,
    normal_axis: usize,
    normal_sign: i32,
    on_floor: bool,
) -> [i32; 3] {
    let fwd_speed = fwd.signum() * speed_q6;
    let right_speed = strafe.signum() * speed_q6;
    let mut intended = [0; 3];
    let mut axis = 0;
    while axis < 3 {
        intended[axis] =
            mul_q12_nearest(forward[axis], fwd_speed) + mul_q12_nearest(right[axis], right_speed);
        axis += 1;
    }

    // PM_LadderMove removes the component through the ladder face, then turns
    // that same component upward. For a vertical cardinal plane,
    // normal x (up x normal) is exactly world-up, so no cross products or
    // normalization are needed at runtime.
    let normal = intended[normal_axis] * normal_sign;
    intended[normal_axis] = 0;
    intended[1] -= normal;
    if on_floor && normal > 0 {
        // Walking away while grounded releases the player from the face.
        intended[normal_axis] = speed_q6 * normal_sign;
    }
    intended
}

#[inline]
fn div_nearest(value: i32, divisor: i32) -> i32 {
    if value >= 0 {
        (value + divisor / 2) / divisor
    } else {
        -((-value + divisor / 2) / divisor)
    }
}

/// Controller-friendly ladder velocity. GoldSrc treats digital strafe as the
/// full 200 u/s climb speed; on a narrow PS1 ladder that crosses the complete
/// attachment width in only a few 20 Hz updates. Preserve the exact GoldSrc
/// forward/climb decomposition while adding one third of the independently
/// decomposed strafe vector. The player can still reposition and deliberately
/// leave sideways, but a brief D-pad/analogue touch no longer ejects them.
#[inline]
pub fn assisted_velocity_q6(
    forward: [i32; 3],
    right: [i32; 3],
    fwd: i32,
    strafe: i32,
    speed_q6: i32,
    normal_axis: usize,
    normal_sign: i32,
    on_floor: bool,
) -> [i32; 3] {
    let forward_velocity = goldsrc_velocity_q6(
        forward,
        right,
        fwd,
        0,
        speed_q6,
        normal_axis,
        normal_sign,
        on_floor,
    );
    let strafe_velocity = goldsrc_velocity_q6(
        forward,
        right,
        0,
        strafe,
        speed_q6,
        normal_axis,
        normal_sign,
        on_floor,
    );
    [
        forward_velocity[0] + div_nearest(strafe_velocity[0], 3),
        forward_velocity[1] + div_nearest(strafe_velocity[1], 3),
        forward_velocity[2] + div_nearest(strafe_velocity[2], 3),
    ]
}

/// Preserve GoldSrc's near-perpendicular ladder feel on the PS1 controller.
/// GoldSrc retains a floating-point hull overlap, while our whole-unit origin
/// can leave a narrow ladder after a single quantized tangential step. Ignore
/// only the small tangent produced by forward input near the face normal;
/// explicit strafe and a deliberately diagonal approach remain untouched.
#[inline]
pub fn lock_small_tangent_q6(
    mut velocity: [i32; 3],
    normal_axis: usize,
    strafe: i32,
    speed_q6: i32,
) -> [i32; 3] {
    let tangent_axis = 2 - normal_axis;
    if strafe == 0 && velocity[tangent_axis].abs() <= speed_q6 / 4 {
        velocity[tangent_axis] = 0;
    }
    velocity
}

/// Infer the cardinal face normal from the already-cooked ladder AABB. BSP
/// `func_ladder` brushes are vertical and axis-aligned in the Half-Life maps,
/// so their thinner horizontal half-extent identifies the plane at no RAM
/// cost. The sign is selected from the player's current side of the volume.
#[inline]
pub fn cardinal_normal(pos: [i32; 3], center: [i32; 3], half: [i32; 3]) -> (usize, i32) {
    if half[0] <= half[2] {
        (0, if pos[0] < center[0] { -1 } else { 1 })
    } else {
        (2, if pos[2] < center[2] { -1 } else { 1 })
    }
}

/// Fixed-point corner contact can reject or truncate a combined ladder move
/// even though the vertical tangent is clear.  GoldSrc's multi-plane slide
/// preserves that tangent; request one vertical-only retry whenever the
/// authored climb fell short.  Retrying only a full block left partial corner
/// ticks climbing half a step, which read as stutter on a straight climb.
#[inline]
pub fn should_retry_vertical(start_y: i32, end_y: i32, wanted_y: i32) -> bool {
    wanted_y != 0 && end_y - start_y != wanted_y
}

/// Accept the vertical-only retry only when it strictly beats the combined
/// slide's progress in the authored direction; never trade real diagonal
/// progress for a shorter vertical result.
#[inline]
pub fn retry_improves_vertical(start_y: i32, slide_y: i32, retry_y: i32, wanted_y: i32) -> bool {
    let dir = wanted_y.signum();
    (retry_y - start_y) * dir > (slide_y - start_y) * dir
}

#[inline]
pub fn mount_target(pos: [i32; 3], center: [i32; 3], half: [i32; 3]) -> [i32; 3] {
    let mut target = pos;
    if half[0] <= half[2] {
        // X is the face normal. Preserve it exactly; moving it even two units
        // away from the t0a0 ladder was the opposite-direction mount kick that
        // does not exist in the captured GoldSrc PM_LadderMove trace.
        target[2] = clamp_inside(pos[2], center[2], half[2]);
    } else {
        // Z is the face normal; clamp only X, the tangential axis.
        target[0] = clamp_inside(pos[0], center[0], half[0]);
    }
    target
}

#[cfg(test)]
mod tests {
    use super::{
        assisted_velocity_q6, axis_speed_nearest, cardinal_normal, goldsrc_velocity_q6,
        lock_small_tangent_q6, mount_target, mul_q12_nearest, retry_improves_vertical,
        should_retry_vertical, touches, touches_attached, touches_mount_assist, wants_descend,
    };

    #[test]
    fn full_semantic_axis_reaches_authored_climb_speed() {
        assert_eq!(axis_speed_nearest(127, 10), 10);
        assert_eq!(axis_speed_nearest(-127, 10), -10);
        assert_eq!(axis_speed_nearest(64, 10), 5);
    }

    #[test]
    fn near_cardinal_lut_values_keep_authored_lateral_speed() {
        assert_eq!(mul_q12_nearest(4095, 10), 10);
        assert_eq!(mul_q12_nearest(-4095, 10), -10);
        assert_eq!(mul_q12_nearest(52, 10), 0);
    }

    #[test]
    fn ladder_contact_keeps_the_tangential_hull_edge() {
        let center = [2183, -67, 723];
        let half = [2, 257, 16];
        assert!(!touches([2174, 133, 690], center, half));
        assert!(touches([2174, 133, 691], center, half));
        assert!(touches([2167, 132, 708], center, half));
        assert!(touches_mount_assist([2175, -267, 756], center, half, false,));
        assert!(!touches_mount_assist([2174, 133, 690], center, half, false));
        assert!(touches_mount_assist([2169, 133, 690], center, half, true));
        assert!(wants_descend(-1000));
        assert!(!wants_descend(0));
    }

    #[test]
    fn mount_assist_never_widens_the_ladder_face() {
        // c1a1d ladder 2 is thin along PSX Z. At Gold y=-423 the standing
        // hull is still one unit short of its -440 face and must first perform
        // the ordinary wall-contact move; the old +2 normal pad grabbed here.
        let center = [1496, 184, -442];
        let half = [12, 168, 2];
        assert!(!touches_mount_assist([1488, 52, -423], center, half, false,));
        assert!(touches_mount_assist([1488, 52, -424], center, half, false,));

        // Preserve the intended two-unit rescue along the wide/tangential
        // edge once the hull has genuinely reached the ladder face.
        assert!(touches_mount_assist([1526, 52, -424], center, half, false,));
        assert!(!touches_mount_assist([1527, 52, -424], center, half, false,));
    }

    #[test]
    fn mounted_contact_tolerates_only_tangential_quantization() {
        let center = [2183, -67, 723];
        let half = [2, 257, 16];
        // X is the thin face axis: never invent contact through the wall gap.
        assert!(!touches_attached([2164, -100, 723], center, half));
        // Z is tangential: retain an established mount for two integer units.
        assert!(touches_attached([2167, -100, 757], center, half));
        assert!(!touches_attached([2167, -100, 758], center, half));
        // The strict top plane still hands movement back to walking physics.
        assert!(!touches_attached([2167, 225, 723], center, half));
    }

    #[test]
    fn ladder_contact_stops_at_the_expanded_hull_boundary() {
        let first_center = [1152, -104, -330];
        let first_half = [12, 120, 2];
        assert!(touches([1149, -188, -314], first_center, first_half));
        assert!(!touches([1149, -188, -311], first_center, first_half));
    }

    #[test]
    fn exact_vertical_exit_plane_releases_for_topout() {
        // c1a1's ladder *105 spans Gold Z -148..805. A standing origin at
        // 805 + 36 is exactly on the expanded hull's top plane and must be
        // handed back to walking physics instead of remaining gravity-free.
        let center = [871, 329, 1947];
        let half = [16, 477, 1];
        assert!(touches([871, 840, 1962], center, half));
        assert!(!touches([871, 841, 1962], center, half));
    }

    #[test]
    fn no_contact_is_invented_across_a_gap_to_the_ladder_volume() {
        let center = [0, 0, 1947];
        let half = [40, 200, 1];
        assert!(touches([0, 0, 1930], center, half));
        assert!(!touches([0, 0, 1929], center, half));
    }

    #[test]
    fn mounts_west_face_and_clamps_tangent() {
        assert_eq!(
            mount_target([2174, -267, 756], [2183, -67, 723], [2, 257, 16]),
            [2174, -267, 738]
        );
    }

    #[test]
    fn mounts_east_face_without_moving_in_range_tangent() {
        assert_eq!(
            mount_target([2200, 10, 720], [2183, -67, 723], [2, 257, 16]),
            [2200, 10, 720]
        );
    }

    #[test]
    fn handles_ladders_thin_along_z() {
        assert_eq!(
            mount_target([95, 7, 80], [100, 0, 100], [12, 40, 2]),
            [95, 7, 80]
        );
    }

    #[test]
    fn t0a0_mount_preserves_goldsrc_ladder_normal_origin() {
        // Authoritative GoldSrc trace: the player begins at Gold Y=764 against
        // the ladder centred at Y=778 and every PM_LadderMove preparation
        // remains on Y=764. The runtime mapping stores Gold Y in PSX Z.
        let pos = [-692, -348, 764];
        assert_eq!(mount_target(pos, [-692, -168, 778], [16, 216, 2]), pos);
    }

    #[test]
    fn retries_any_shortfall_of_the_authored_vertical_move() {
        assert!(should_retry_vertical(143, 143, -10));
        assert!(should_retry_vertical(10, 10, 10));
        // A partial corner slide is a shortfall too: retrying only the full
        // block left those ticks climbing 1 unit instead of 10.
        assert!(should_retry_vertical(143, 142, -10));
        assert!(should_retry_vertical(100, 107, 14));
        assert!(!should_retry_vertical(143, 133, -10));
        assert!(!should_retry_vertical(100, 114, 14));
        assert!(!should_retry_vertical(143, 143, 0));
    }

    #[test]
    fn retry_replaces_only_a_slide_it_strictly_beats() {
        assert!(retry_improves_vertical(100, 100, 114, 14));
        assert!(retry_improves_vertical(100, 107, 114, 14));
        assert!(!retry_improves_vertical(100, 107, 107, 14));
        assert!(!retry_improves_vertical(100, 107, 100, 14));
        // Downward climbs compare in the authored direction.
        assert!(retry_improves_vertical(100, 100, 90, -10));
        assert!(!retry_improves_vertical(100, 93, 95, -10));
    }

    #[test]
    fn terminal_fall_speed_cannot_tunnel_the_vertical_touch_window() {
        // sv_maxvelocity caps a fall at 100 units per 20 Hz tick, while the
        // vertical acceptance band spans 2*(half+34) >= 100 units for any
        // ladder at least 16 units tall: consecutive whole-unit origins can
        // never straddle the volume, so airborne mounts need no swept test.
        let center = [0, 0, 0];
        let half = [2, 16, 16];
        let mut start_y = 160;
        while start_y > 60 {
            let mut y = start_y;
            let mut crossed = false;
            while y > -160 {
                crossed |= touches([0, y, 10], center, half);
                y -= 100;
            }
            assert!(crossed, "fall from {start_y} skipped the touch window");
            start_y -= 1;
        }
    }

    #[test]
    fn goldsrc_turns_wallward_view_motion_into_vertical_climb() {
        // Facing straight into a +Z ladder while looking up 45 degrees. The
        // 7.08-unit view rise and 7.08-unit wallward component sum to the
        // characteristic 14.16-unit GoldSrc ladder step.
        assert_eq!(
            goldsrc_velocity_q6([0, 2896, -2896], [-4096, 0, 0], 127, 0, 640, 2, 1, false,),
            [0, 906, 0]
        );
    }

    #[test]
    fn c1a1d_ladder_keeps_only_the_small_authored_tangent() {
        // Approximate Q12 vectors at the captured yaw=-91.38, pitch=-49.71.
        // The old generic wall drift became -1 whole X unit every tick; the
        // GoldSrc decomposition retains -10/64 u/tick and error-diffuses it.
        assert_eq!(
            goldsrc_velocity_q6([-64, 3124, -2639], [-4095, 0, 99], 127, 0, 640, 2, 1, false,),
            [-10, 900, 0]
        );
        assert_eq!(
            cardinal_normal([1148, -188, -316], [1152, -104, -330], [12, 120, 2]),
            (2, 1)
        );
    }

    #[test]
    fn near_normal_forward_input_locks_without_eating_intentional_strafe() {
        let small_drift = [-10, 900, 0];
        assert_eq!(lock_small_tangent_q6(small_drift, 2, 0, 640), [0, 900, 0]);
        assert_eq!(lock_small_tangent_q6(small_drift, 2, -1, 640), small_drift);

        // More than one quarter of the authored speed is a deliberate
        // diagonal traversal, not camera/controller alignment noise.
        let diagonal = [-161, 820, 0];
        assert_eq!(lock_small_tangent_q6(diagonal, 2, 0, 640), diagonal);
    }

    #[test]
    fn assisted_strafe_is_one_third_speed_without_reducing_climb() {
        let forward = [0, 2896, -2896];
        let right = [-4096, 0, 0];
        assert_eq!(
            assisted_velocity_q6(forward, right, 127, 127, 640, 2, 1, false),
            [-213, 906, 0]
        );
        assert_eq!(
            assisted_velocity_q6(forward, right, 127, 0, 640, 2, 1, false),
            [0, 906, 0]
        );
    }
}
