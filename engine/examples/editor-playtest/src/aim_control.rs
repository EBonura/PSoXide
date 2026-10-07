//! Held aim is a temporary camera layer; target ownership remains with R3.
//! Offsets use Q12 turns with eight fractional bits, at the fixed 60 Hz tick.
#[derive(Clone, Copy, Default)]
pub(super) struct AimControl {
    offset: [i32; 2],
    idle_ticks: u8,
    camera_was_locked_aim: bool,
}

impl AimControl {
    pub const EMPTY: Self = Self {
        offset: [0; 2],
        idle_ticks: 0,
        camera_was_locked_aim: false,
    };

    pub fn tick(&mut self, aiming: bool, locked: bool, axes: (i16, i16)) {
        if !aiming || !locked {
            self.offset = [0; 2];
            self.idle_ticks = 0;
            return;
        }
        if axes != (0, 0) {
            self.idle_ticks = 0;
            for ((value, axis), limit) in self
                .offset
                .iter_mut()
                .zip([axes.0, axes.1])
                .zip([68 * 256, 46 * 256])
            {
                let mut delta = i32::from(axis) * 5 * 256 / 128;
                // Resistance only opposes outward motion, never correction.
                if delta.signum() == value.signum() {
                    let remaining = (limit - value.abs()).max(0);
                    delta = delta * remaining.min(limit / 3) / (limit / 3);
                }
                *value = (*value + delta).clamp(-limit, limit);
            }
        } else {
            self.idle_ticks = self.idle_ticks.saturating_add(1);
            if self.idle_ticks > 9 {
                for value in &mut self.offset {
                    // 90% return in ~250 ms; monotonic, without overshoot.
                    *value -= (*value / 6) + value.signum();
                }
            }
        }
    }

    pub fn angles(&self) -> [i16; 2] {
        [(self.offset[0] / 256) as i16, (self.offset[1] / 256) as i16]
    }

    /// Both explicit unlock and a disappearing enemy preserve the aim view.
    pub fn camera_transition(&mut self, aiming: bool, locked: bool) -> bool {
        let preserve = aiming && !locked && self.camera_was_locked_aim;
        self.camera_was_locked_aim = aiming && locked;
        preserve
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tether_is_bounded_resists_outward_motion_and_never_fights_input() {
        let mut aim = AimControl::EMPTY;
        for _ in 0..120 {
            aim.tick(true, true, (128, -128));
        }
        let full = aim.angles();
        assert!((65..=68).contains(&full[0]));
        assert!((-46..=-43).contains(&full[1]));
        aim.tick(true, true, (-128, 128));
        assert!(aim.angles()[0] < full[0] - 3);
        assert!(aim.angles()[1] > full[1] + 3);
        let held = aim.angles();
        for _ in 0..9 {
            aim.tick(true, true, (0, 0));
        }
        assert_eq!(aim.angles(), held);
        for _ in 0..18 {
            aim.tick(true, true, (0, 0));
        }
        assert!(aim.angles()[0].abs() <= 3 && aim.angles()[1].abs() <= 3);
        for _ in 0..90 {
            aim.tick(true, true, (0, 0));
        }
        assert_eq!(aim.angles(), [0, 0]);
    }
    #[test]
    fn lock_and_held_aim_are_independent_and_loss_preserves_view_once() {
        let mut aim = AimControl::EMPTY;
        for (aiming, locked, preserve) in [
            (false, false, false),
            (false, true, false),
            (true, true, false),
            (false, true, false),
            (true, true, false),
            (true, false, true),
            (true, false, false),
            (true, true, false),
            (true, false, true),
            (false, false, false),
        ] {
            assert_eq!(aim.camera_transition(aiming, locked), preserve);
        }
        aim.tick(true, true, (128, 128));
        aim.tick(false, true, (128, 128));
        assert_eq!(aim.angles(), [0, 0]);
        aim.tick(true, false, (128, 128));
        assert_eq!(aim.angles(), [0, 0]);
    }
}
