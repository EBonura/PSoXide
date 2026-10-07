//! Fixed-duration hook traversal and deterministic screen-space acquisition.
use psx_engine::RoomPoint;

/// Visible anticipation before the polygon burst.
pub const LAUNCH_TICKS: u32 = 12;
/// Leave three capture ticks after anticipation before moving.
pub const DEPARTURE_TICKS: u32 = LAUNCH_TICKS + 3;
/// Body reconstruction begins after the wire reaches the destination.
pub const TRAVEL_TICKS: u32 = LAUNCH_TICKS + 30;
/// Fixed cap keeps per-tick visibility and collision work bounded.
pub const MAX_HOOK_POINTS: usize = psx_level::MAX_HOOK_POINTS;
/// Maximum acquisition distance in runtime world units.
pub const MAX_RANGE: i32 = 1600;
/// Visual anchor height above the landing.
pub const MARKER_HEIGHT: i32 = 96;
/// Acquisition radius around the reticle, in native screen pixels.
pub const AIM_RADIUS: i32 = 30;

/// One flight, driven by the fixed simulation clock.
#[derive(Clone, Copy, Debug)]
pub struct HookTravel {
    /// Departure foot position.
    pub start: RoomPoint,
    /// Validated destination foot position.
    pub landing: RoomPoint,
    /// Start simulation tick.
    pub started: u32,
}
impl HookTravel {
    /// Fixed trailing heading from the departure-to-landing bearing. Using the
    /// original endpoints avoids a flip or lost heading on the vertical descent.
    pub fn camera_yaw(&self, fallback: psx_engine::Angle) -> psx_engine::Angle {
        psx_engine::yaw_to_point(self.start, self.landing)
            .map(|yaw| yaw.add(psx_engine::Angle::HALF))
            .unwrap_or(fallback)
    }
    /// Direct attraction to the perch avoids an artificial apex hitting indoor ceilings.
    pub fn position(&self, now: u32) -> RoomPoint {
        let age = now.wrapping_sub(self.started).min(TRAVEL_TICKS);
        let t = age.saturating_sub(DEPARTURE_TICKS) as i32;
        let duration = (TRAVEL_TICKS - DEPARTURE_TICKS) as i32;
        let lerp = |a: i32, b: i32| a + (b - a) * t / duration;
        RoomPoint::new(
            lerp(self.start.x, self.landing.x),
            lerp(self.start.y, self.landing.y),
            lerp(self.start.z, self.landing.z),
        )
    }
    /// Whether the wire has reached the landing.
    pub fn finished(&self, now: u32) -> bool {
        now.wrapping_sub(self.started) >= TRAVEL_TICKS
    }
}

/// Perches within a bounded radius are eligible, including lower arches.
/// The caller additionally proves visibility and body clearance.
pub fn in_range(player: RoomPoint, landing: RoomPoint) -> bool {
    let dx = landing.x.saturating_sub(player.x);
    let dy = landing.y.saturating_sub(player.y);
    let dz = landing.z.saturating_sub(player.z);
    if (dx == 0 && dy == 0 && dz == 0)
        || [dx, dy, dz].iter().any(|v| v.saturating_abs() > MAX_RANGE)
    {
        return false;
    }
    dx * dx + dy * dy + dz * dz <= MAX_RANGE * MAX_RANGE
}

/// Squared reticle distance for an eligible projected hook.
pub fn aim_score(x: i32, y: i32, aim_x: i32, aim_y: i32) -> Option<i32> {
    let dx = x - aim_x;
    let dy = y - aim_y;
    let score = dx * dx + dy * dy;
    (score <= AIM_RADIUS * AIM_RADIUS).then_some(score)
}

/// Small anticipation squeeze, then a wider flight lens. Zero is the player's
/// normal lens; negative values shorten focal length and widen field of view.
pub fn lens_target_q8(age: Option<u32>) -> i32 {
    match age {
        Some(age) if age < LAUNCH_TICKS => 8,
        Some(_) => -52,
        None => 0,
    }
}

/// Bounded lens easing also settles cancelled moves back to the base profile.
pub fn ease_lens_q8(current: i32, target: i32) -> i32 {
    current + (target - current).clamp(-6, 6)
}

/// Authored left-hand contact relative to the motor root (Aletha ArchPerch v1).
pub const PERCH_HAND: [i32; 3] = [12, 81, -33];
/// Quantized aiming lattice: five yaw columns and five pitch rows, two recoil banks.
pub const PERCH_FRAMES: u16 = 50;

/// Two adjacent pitch rows, each interpolated horizontally by the asset sampler.
/// Angles are Q12 turns; the returned phases never interpolate into another row.
pub fn perch_aim_sample(yaw: i32, pitch: i32, recoil: bool) -> (u32, u32, u16) {
    let x = ((yaw.clamp(-910, 910) + 910) * 4 * 4096 / 1820) as u32;
    let y = ((pitch.clamp(-683, 683) + 683) * 4 * 4096 / 1366) as u32;
    let row = (y >> 12).min(4);
    let next = (row + 1).min(4);
    let bank = if recoil { 25 } else { 0 };
    (
        ((bank + row * 5) << 12) + x,
        ((bank + next * 5) << 12) + x,
        (y & 4095) as u16,
    )
}
/// One optional empowered shot per grounded visit; ordinary shots remain unlimited.
pub const CHARGE_TICKS: u16 = 75;

/// Prototype motion: tower and duel arches move laterally over eight seconds.
pub fn arch_offset(slot: usize, tick: u32) -> i32 {
    if slot != 1 && slot != 3 {
        return 0;
    }
    psx_math::sin_q12(((tick % 480) * 4096 / 480) as u16) * 64 / 4096
}

/// Charge cannot be farmed by detaching and immediately reattaching in midair.
#[derive(Clone, Copy, Debug, Default)]
pub struct ArchCharge {
    ticks: u16,
    spent: bool,
}
impl ArchCharge {
    /// Fresh visit with no accumulated charge.
    pub const EMPTY: Self = Self {
        ticks: 0,
        spent: false,
    };
    /// Advance charge or rearm after touching solid ground.
    pub fn tick(&mut self, attached: bool, grounded: bool) {
        if grounded {
            *self = Self::EMPTY;
        } else if attached && !self.spent {
            self.ticks = self.ticks.saturating_add(1).min(CHARGE_TICKS);
        } else if !attached {
            self.ticks = 0;
        }
    }
    /// Whether the next successfully spawned shot receives the bonus.
    pub fn ready(&self) -> bool {
        !self.spent && self.ticks == CHARGE_TICKS
    }
    /// Spend the reward once, after projectile allocation succeeds.
    pub fn consume(&mut self) {
        if self.ready() {
            self.spent = true;
            self.ticks = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn perch_samples_stay_inside_their_rows_and_banks() {
        for recoil in [false, true] {
            for yaw in [-4096, -910, -455, 0, 455, 910, 4096] {
                for pitch in [-4096, -683, 0, 683, 4096] {
                    let (a, b, w) = perch_aim_sample(yaw, pitch, recoil);
                    assert!(a <= b && b < u32::from(PERCH_FRAMES) * 4096);
                    assert!(w < 4096);
                    assert_eq!((a >> 12) / 25, u32::from(recoil));
                    assert_eq!((b >> 12) / 25, u32::from(recoil));
                }
            }
        }
        assert_eq!(perch_aim_sample(0, 0, false), (12 << 12, 17 << 12, 0));
    }
    #[test]
    fn charge_requires_ground_between_rewards() {
        let mut c = ArchCharge::EMPTY;
        for _ in 0..CHARGE_TICKS {
            c.tick(true, false);
        }
        assert!(c.ready());
        c.consume();
        c.tick(false, false);
        for _ in 0..CHARGE_TICKS * 2 {
            c.tick(true, false);
        }
        assert!(!c.ready());
        c.tick(false, true);
        for _ in 0..CHARGE_TICKS {
            c.tick(true, false);
        }
        assert!(c.ready());
        c.tick(false, false);
        assert!(!c.ready());
    }
    #[test]
    fn moving_arch_is_bounded_and_periodic() {
        for t in 0..960 {
            assert!(arch_offset(1, t).abs() <= 64);
            assert_eq!(arch_offset(1, t), arch_offset(1, t + 480));
            assert_eq!(arch_offset(0, t), 0);
        }
        assert!(in_range(RoomPoint::new(0, 200, 0), RoomPoint::ZERO));
    }
    #[test]
    fn camera_heading_follows_route_and_retains_vertical_fallback() {
        use psx_engine::Angle;
        let mut flight = HookTravel {
            start: RoomPoint::ZERO,
            landing: RoomPoint::new(200, 200, 200),
            started: 0,
        };
        let expected = psx_engine::yaw_to_point(flight.start, flight.landing)
            .unwrap()
            .add(Angle::HALF);
        assert_eq!(flight.camera_yaw(Angle::ZERO), expected);
        assert_eq!(flight.camera_yaw(Angle::HALF), expected);
        flight.landing = RoomPoint::new(0, 200, 0);
        assert_eq!(flight.camera_yaw(expected), expected);
    }
    #[test]
    fn launch_lens_widens_then_restores_without_overshoot() {
        let mut lens = 0;
        for _ in 0..12 {
            lens = ease_lens_q8(lens, lens_target_q8(Some(0)));
        }
        assert_eq!(lens, 8);
        for _ in 0..12 {
            lens = ease_lens_q8(lens, lens_target_q8(Some(LAUNCH_TICKS)));
        }
        assert_eq!(lens, -52);
        for _ in 0..12 {
            lens = ease_lens_q8(lens, lens_target_q8(None));
        }
        assert_eq!(lens, 0);
    }
    #[test]
    fn elevated_range_and_aim_are_bounded() {
        assert!(in_range(RoomPoint::ZERO, RoomPoint::new(400, 192, 200)));
        for p in [
            RoomPoint::ZERO,
            RoomPoint::new(1600, 32, 0),
            RoomPoint::new(i32::MAX, 64, 0),
        ] {
            assert!(!in_range(RoomPoint::ZERO, p));
        }
        assert_eq!(aim_score(160, 120, 160, 120), Some(0));
        assert_eq!(aim_score(190, 120, 160, 120), Some(900));
        assert_eq!(aim_score(191, 120, 160, 120), None);
    }
    #[test]
    fn departure_holds_then_rises_and_lands_exactly_without_overshoot() {
        let f = HookTravel {
            start: RoomPoint::new(-100, 0, -400),
            landing: RoomPoint::new(300, 192, 100),
            started: 100,
        };
        assert_eq!(f.position(115), f.start);
        assert!(f.position(135).y < f.landing.y);
        assert_eq!(f.position(142), f.landing);
        assert_eq!(f.position(150), f.landing);
        for t in 116..135 {
            assert!(f.position(t).y > f.start.y);
            assert!(f.position(t).y <= f.landing.y);
        }
        assert!(!f.finished(141));
        assert!(f.finished(142));
    }
}
