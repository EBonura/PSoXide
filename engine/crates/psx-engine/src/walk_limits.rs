//! The numbers that decide where a character can walk.
//!
//! One source for the character motor and for anything that predicts the
//! motor: the grid editor's walkability overlay, the Ramp and Stairs verbs and
//! the cook's walkable-slope bit all read [`WalkLimits::DEFAULT`], so an edit
//! to a limit moves the game and the tools together. Every value is an
//! integer in runtime units (or Q12 for the slope).

/// Limits of walking, in runtime units.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WalkLimits {
    /// Tallest riser the body steps up instead of being blocked by.
    ///
    /// Sits in the gap between demo-scale steps (about 576 or less) and real
    /// walls (about 1152 or more) in the motor's older unit.
    pub step_up: i32,
    /// Largest drop the feet snap straight down to; a bigger one leaves the
    /// body airborne.
    pub step_down: i32,
    /// Height of the body's collision cylinder: the least headroom a
    /// character fits under.
    pub body_height: i32,
    /// Smallest upward normal component, Q12, a surface may have and still
    /// be stood on.
    ///
    /// Quake's rule: a plane is floor when its upward normal component is at
    /// least 0.7, roughly 45.6 degrees; steeper is a wall the body slides
    /// down. 0.7 * 4096 = 2867.
    pub min_floor_normal_y_q12: i16,
}

impl WalkLimits {
    /// What the character motor uses today.
    pub const DEFAULT: Self = Self {
        step_up: 40,
        step_down: 40,
        body_height: 48,
        min_floor_normal_y_q12: 2867,
    };
}

impl Default for WalkLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_motors_numbers() {
        let limits = WalkLimits::DEFAULT;
        assert_eq!(limits.step_up, 40);
        assert_eq!(limits.step_down, 40);
        assert_eq!(limits.body_height, 48);
        // 0.7 in Q12, rounded down.
        assert_eq!(i32::from(limits.min_floor_normal_y_q12), 2867);
    }
}
