//! Shared cover/peek decisions. World queries and legal movement belong to the caller.
use crate::combat_flow::{
    EVADE_COOLDOWN_TICKS, EVADE_DISTANCE, EVADE_MOVE_TICKS, EVADE_RECOVERY_TICKS,
};
use crate::projectiles::ProjectileThreat;

/// Mode: in the open, free to fire.
pub const OPEN: u8 = 0;
/// Mode: moving toward a cover point.
pub const SEEK_COVER: u8 = 1;
/// Mode: waiting behind cover.
pub const COVERED: u8 = 2;
/// Mode: stepping out of cover to take a shot.
pub const PEEK: u8 = 3;
/// Mode: sidestepping an incoming projectile.
pub const EVADE: u8 = 4;
/// Mode: scanning for a new cover point.
pub const SEARCH: u8 = 5;

/// What the caller should do this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangedOrder {
    /// World position to walk toward.
    pub destination: [i32; 3],
    /// True when the actor should move to `destination`.
    pub moving: bool,
    /// True when the actor may start a shot.
    pub fire: bool,
    /// True while a projectile sidestep is active.
    pub evade: bool,
}

/// Fixed storage, including an incremental search rather than an unbounded frame spike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangedExchange {
    /// Current mode, one of the mode constants in this module.
    pub mode: u8,
    cover: [i32; 3],
    peek: [i32; 3],
    destination: [i32; 3],
    timer: u16,
    decision_delay: u16,
    evade_delay: u16,
    last_energy: u16,
    shots: u8,
    candidate: u8,
    side: i32,
}
impl RangedExchange {
    /// Open-field state with no timers running.
    pub const EMPTY: Self = Self {
        mode: OPEN,
        cover: [0; 3],
        peek: [0; 3],
        destination: [0; 3],
        timer: 0,
        decision_delay: 0,
        evade_delay: 0,
        last_energy: 100,
        shots: 0,
        candidate: 0,
        side: 1,
    };
    /// Advance the timers by `delta` ticks.
    pub fn tick(&mut self, delta: u16) {
        self.timer = self.timer.saturating_sub(delta);
        self.decision_delay = self.decision_delay.saturating_sub(delta);
        self.evade_delay = self.evade_delay.saturating_sub(delta);
    }
    /// Return to the open state; the sidestep cooldown carries over.
    pub fn reset(&mut self) {
        let cooldown = self.evade_delay;
        *self = Self::EMPTY;
        self.evade_delay = cooldown;
    }
    /// The order implied by the current mode, without making a new decision.
    pub fn current_order(&self, from: [i32; 3]) -> RangedOrder {
        self.order(from)
    }
    fn enter(&mut self, mode: u8, ticks: u16, destination: [i32; 3]) {
        self.mode = mode;
        self.timer = ticks;
        self.destination = destination;
    }
    fn order(&self, from: [i32; 3]) -> RangedOrder {
        RangedOrder {
            destination: self.destination,
            moving: matches!(self.mode, SEEK_COVER | PEEK | EVADE | SEARCH)
                && (self.mode != EVADE || self.timer > EVADE_RECOVERY_TICKS)
                && distance(from, self.destination) > 4,
            fire: self.mode == OPEN,
            evade: self.mode == EVADE,
        }
    }
    /// Observe released projectiles independently of cover's slower decision cadence.
    /// An active sidestep includes its vulnerable recovery; no attack can start in it.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub fn try_evade(
        &mut self,
        from: [i32; 3],
        radius: i32,
        height: i32,
        threat: Option<ProjectileThreat>,
        clear: &mut dyn FnMut([i32; 3], [i32; 3]) -> bool,
    ) -> bool {
        if self.mode == EVADE && self.timer != 0 {
            return true;
        }
        if let Some(threat) =
            threat.filter(|t| self.evade_delay == 0 && clear(eye(from, height), t.position))
        {
            let v = threat.velocity;
            for sign in [self.side, -self.side] {
                let to = offset(from, [-v[2] * sign, v[0] * sign], EVADE_DISTANCE);
                if lane(from, to, radius, height, clear) {
                    self.side = sign;
                    self.enter(EVADE, EVADE_MOVE_TICKS + EVADE_RECOVERY_TICKS, to);
                    self.evade_delay = EVADE_COOLDOWN_TICKS;
                    return true;
                }
            }
        }
        false
    }
    /// Queries describe current geometry, not a cover tag or distance heuristic.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub fn decide(
        &mut self,
        from: [i32; 3],
        target: [i32; 3],
        radius: i32,
        height: i32,
        far: i32,
        energy: u16,
        threat: Option<ProjectileThreat>,
        clear: &mut dyn FnMut([i32; 3], [i32; 3]) -> bool,
    ) -> RangedOrder {
        if energy < self.last_energy {
            self.shots = self.shots.saturating_add(1);
        }
        self.last_energy = energy;
        if self.try_evade(from, radius, height, threat, clear) {
            return self.order(from);
        }
        if self.mode == EVADE {
            self.decision_delay = 0;
        }
        if self.decision_delay != 0 {
            return self.order(from);
        }
        self.decision_delay = 12;
        match self.mode {
            EVADE if self.timer == 0 => {
                self.enter(SEARCH, 144, from);
                self.candidate = 0;
            }
            EVADE => return self.order(from),
            SEEK_COVER if distance(from, self.cover) <= 4 => {
                // Arrival is only cover while geometry still conceals this actor.
                if concealed(from, target, radius, height, clear) {
                    self.enter(COVERED, 54, from);
                } else {
                    self.enter(SEARCH, 144, from);
                }
            }
            SEEK_COVER if self.timer == 0 => self.enter(SEARCH, 144, from),
            COVERED if (self.timer == 0 || !concealed(from, target, radius, height, clear)) => {
                self.enter(PEEK, 120, self.peek);
            }
            PEEK => {
                if distance(from, self.peek) <= 4 && clear(eye(from, height), eye(target, height)) {
                    self.shots = 0;
                    self.enter(OPEN, 120, from);
                } else if self.timer == 0 {
                    self.enter(SEARCH, 144, from);
                }
            }
            OPEN if self.shots >= 2 || self.timer == 0 => {
                self.enter(SEARCH, 144, from);
                self.candidate = 0;
            }
            _ => {}
        }
        if self.mode == SEARCH {
            // At most two cover candidates each 0.2-second observation.
            const DIRS: [[i32; 2]; 8] = [
                [1, 0],
                [-1, 0],
                [0, 1],
                [0, -1],
                [1, 1],
                [-1, 1],
                [1, -1],
                [-1, -1],
            ];
            for _ in 0..2 {
                let n = usize::from(self.candidate % 24);
                self.candidate = self.candidate.saturating_add(1);
                let candidate = offset(from, DIRS[n % 8], 64 + (n / 8) as i32 * 64);
                if distance(candidate, target) < 100
                    || distance(candidate, target) > far + 96
                    || !lane(from, candidate, radius, height, clear)
                    || !concealed(candidate, target, radius, height, clear)
                {
                    continue;
                }
                let radial = [candidate[0] - target[0], candidate[2] - target[2]];
                for sign in [self.side, -self.side] {
                    for width in [48, 96] {
                        let peek = offset(candidate, [-radial[1] * sign, radial[0] * sign], width);
                        if lane(candidate, peek, radius, height, clear)
                            && clear(eye(peek, height), eye(target, height))
                        {
                            self.cover = candidate;
                            self.peek = peek;
                            self.side = sign;
                            self.enter(SEEK_COVER, 180, candidate);
                            return self.order(from);
                        }
                    }
                }
            }
            if self.timer == 0 || self.candidate >= 24 {
                // No real cover: never pretend that standing still is protection.
                self.shots = 0;
                self.enter(OPEN, 120, from);
            } else {
                let away = [from[0] - target[0], from[2] - target[2]];
                let to = offset(from, [-away[1] * self.side, away[0] * self.side], 48);
                if lane(from, to, radius, height, clear) {
                    self.destination = to;
                } else {
                    self.side = -self.side;
                    self.destination = from;
                }
            }
        }
        self.order(from)
    }
}
fn eye(mut p: [i32; 3], height: i32) -> [i32; 3] {
    p[1] += height * 3 / 4;
    p
}
fn distance(a: [i32; 3], b: [i32; 3]) -> i32 {
    (a[0] - b[0]).abs().max((a[2] - b[2]).abs())
}
#[cfg_attr(target_arch = "mips", optimize(size))]
fn offset(p: [i32; 3], v: [i32; 2], amount: i32) -> [i32; 3] {
    let len = psx_math::int32::isqrt_i32(
        psx_math::int32::square_i32_saturating(v[0])
            .saturating_add(psx_math::int32::square_i32_saturating(v[1])),
    )
    .max(1);
    [p[0] + v[0] * amount / len, p[1], p[2] + v[1] * amount / len]
}
#[cfg_attr(target_arch = "mips", optimize(size))]
fn lane(
    from: [i32; 3],
    to: [i32; 3],
    radius: i32,
    height: i32,
    clear: &mut dyn FnMut([i32; 3], [i32; 3]) -> bool,
) -> bool {
    let v = [to[0] - from[0], to[2] - from[2]];
    let side = offset([0; 3], [-v[1], v[0]], radius + 4);
    [-1, 1].into_iter().all(|s| {
        clear(
            eye(
                [from[0] + side[0] * s, from[1], from[2] + side[2] * s],
                height,
            ),
            eye([to[0] + side[0] * s, to[1], to[2] + side[2] * s], height),
        )
    })
}
#[cfg_attr(target_arch = "mips", optimize(size))]
fn concealed(
    p: [i32; 3],
    target: [i32; 3],
    radius: i32,
    height: i32,
    clear: &mut dyn FnMut([i32; 3], [i32; 3]) -> bool,
) -> bool {
    // Conceal the body's width with a small arrival margin, not only its centre.
    let side = offset([0; 3], [-(p[2] - target[2]), p[0] - target[0]], radius + 8);
    [height / 3, height * 3 / 4].into_iter().all(|y| {
        [-1, 1].into_iter().all(|s| {
            !clear(
                eye(target, height),
                [p[0] + side[0] * s, p[1] + y, p[2] + side[2] * s],
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pillar(a: [i32; 3], b: [i32; 3]) -> bool {
        (0..=64).all(|n| {
            let x = a[0] + (b[0] - a[0]) * n / 64;
            let z = a[2] + (b[2] - a[2]) * n / 64;
            !(40..=88).contains(&x) || !(96..=144).contains(&z)
        })
    }
    #[test]
    fn evade_has_recovery_and_cooldown_survives_stance_reset() {
        let mut e = RangedExchange::EMPTY;
        let threat = Some(ProjectileThreat {
            position: [0, 36, 160],
            velocity: [0, 0, -8],
            ticks_to_contact: 20,
        });
        assert!(e.try_evade([0; 3], 12, 72, threat, &mut |_, _| true));
        e.tick(EVADE_MOVE_TICKS);
        let recovery = e.current_order([20, 0, 0]);
        assert!(recovery.evade);
        assert!(!recovery.moving && !recovery.fire);
        e.tick(EVADE_RECOVERY_TICKS);
        e.reset();
        assert!(!e.try_evade([0; 3], 12, 72, threat, &mut |_, _| true));
        e.tick(EVADE_COOLDOWN_TICKS - EVADE_MOVE_TICKS - EVADE_RECOVERY_TICKS);
        assert!(e.try_evade([0; 3], 12, 72, threat, &mut |_, _| true));
    }
    #[test]
    fn evade_chooses_the_other_lane_when_first_side_is_blocked() {
        let mut e = RangedExchange::EMPTY;
        let threat = Some(ProjectileThreat {
            position: [0, 36, 160],
            velocity: [0, 0, -8],
            ticks_to_contact: 20,
        });
        assert!(e.try_evade([0; 3], 12, 72, threat, &mut |_, b| b[0] <= 32));
        assert!(e.current_order([0; 3]).destination[0] < 0);
        e = RangedExchange::EMPTY;
        assert!(!e.try_evade([0; 3], 12, 72, threat, &mut |_, b| b[0].abs() < 32));
    }
    #[test]
    fn hides_behind_actual_geometry_then_peeks_before_firing() {
        let mut exchange = RangedExchange::EMPTY;
        exchange.timer = 0;
        let mut p = [0, 0, 0];
        let target = [0, 0, 224];
        let mut covered = false;
        let mut peeked = false;
        let mut fired = false;
        for _ in 0..60 {
            exchange.tick(12);
            let o = exchange.decide(p, target, 12, 72, 256, 100, None, &mut pillar);
            if exchange.mode == COVERED {
                covered = true;
                assert!(!o.fire);
                assert!(!pillar(eye(p, 72), eye(target, 72)));
            }
            peeked |= covered && exchange.mode == PEEK;
            if peeked && o.fire {
                assert!(pillar(eye(p, 72), eye(target, 72)));
                fired = true;
                break;
            }
            if o.moving {
                assert!(lane(p, o.destination, 12, 72, &mut pillar));
                p = o.destination;
            }
        }
        assert!(covered && peeked && fired);
    }
    #[test]
    fn open_ground_never_counts_as_cover_and_search_is_bounded() {
        let mut exchange = RangedExchange::EMPTY;
        exchange.timer = 0;
        for _ in 0..13 {
            exchange.tick(12);
            exchange.decide([0; 3], [0, 0, 224], 12, 72, 256, 100, None, &mut |_, _| {
                true
            });
            assert_ne!(exchange.mode, COVERED);
            assert_ne!(exchange.mode, SEEK_COVER);
        }
        assert_eq!(exchange.mode, OPEN);
    }
    #[test]
    fn evasions_require_visible_live_threats_and_have_a_cooldown() {
        let threat = Some(ProjectileThreat {
            position: [0, 36, 160],
            velocity: [0, 0, -8],
            ticks_to_contact: 20,
        });
        let mut e = RangedExchange::EMPTY;
        let o = e.decide(
            [0; 3],
            [0, 0, 224],
            12,
            72,
            256,
            100,
            threat,
            &mut |_, _| true,
        );
        assert!(o.evade && o.moving);
        assert_ne!(o.destination[0], 0);
        e.tick(30);
        assert!(
            !e.decide(
                o.destination,
                [0, 0, 224],
                12,
                72,
                256,
                100,
                threat,
                &mut |_, _| true
            )
            .evade
        );
        e.reset();
        assert!(
            !e.decide(
                [0; 3],
                [0, 0, 224],
                12,
                72,
                256,
                100,
                threat,
                &mut |_, _| false
            )
            .evade
        );
    }
}
