//! Small shared posture meter for player and enemy hit interruption.

/// Quiet time before accumulated posture damage clears (60 Hz ticks).
pub const POISE_RESET_TICKS: u16 = 120;

/// No allocation, and zero-initializable for the guest's entity arrays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Poise {
    damage: u16,
    quiet_ticks: u16,
}

impl Poise {
    /// Fresh posture.
    pub const EMPTY: Self = Self {
        damage: 0,
        quiet_ticks: 0,
    };

    /// Recover after a full quiet interval, independently of update cadence.
    pub fn tick(&mut self, delta: u16) {
        self.quiet_ticks = self.quiet_ticks.saturating_sub(delta);
        if self.quiet_ticks == 0 {
            self.damage = 0;
        }
    }

    /// Apply posture damage. Armor doubles resistance only during an authored
    /// heavy swing's active window; it never grants immunity. Equality breaks.
    pub fn hit(&mut self, amount: u16, capacity: u16, armored: bool) -> bool {
        if amount == 0 {
            return false;
        }
        self.quiet_ticks = POISE_RESET_TICKS;
        self.damage = self.damage.saturating_add(amount);
        let limit = if armored {
            capacity.saturating_mul(2)
        } else {
            capacity
        }
        .max(1);
        if self.damage >= limit {
            *self = Self::EMPTY;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn light_hits_interrupt_and_heavy_posture_needs_a_combo() {
        assert!(Poise::EMPTY.hit(25, 25, false));
        let mut heavy = Poise::EMPTY;
        assert!(!heavy.hit(25, 50, false));
        assert!(heavy.hit(25, 50, false));
        assert!(Poise::EMPTY.hit(50, 50, false));
    }
    #[test]
    fn heavy_active_armor_is_finite_and_quiet_time_resets_posture() {
        let mut p = Poise::EMPTY;
        assert!(!p.hit(50, 50, true));
        assert!(p.hit(50, 50, true));
        assert!(!p.hit(25, 50, false));
        p.tick(119);
        assert_ne!(p, Poise::EMPTY);
        p.tick(1);
        assert!(!p.hit(25, 50, false));
        assert!(!p.hit(0, 0, false));
    }
}
