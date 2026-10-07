//! Shared tactical choices. Controllers execute intents through their own legal actions.
use crate::vitality::VitalityChannelId;

/// Why the policy prefers a stance. Stable values are used by duel reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StanceReason {
    /// Match the current separation with hysteresis.
    Distance = 0,
    /// Leave a depleted active pool while a healthy reserve is available.
    RecoverPool = 1,
    /// Commit to closing after repeated shots.
    PressAfterShots = 2,
    /// Pressure a visible exposed pool rather than repeatedly chipping the guard.
    ExposedChannel = 3,
}

/// Distance hysteresis prevents stance oscillation at a single threshold.
pub fn melee_limit(entry: i32, tolerance: i32, was_melee: bool) -> i32 {
    entry.saturating_add(if was_melee { tolerance } else { 0 })
}

/// Select the distance-based family using the shared hysteresis limit.
pub fn wants_melee(distance: i32, entry: i32, tolerance: i32, was_melee: bool) -> bool {
    distance <= melee_limit(entry, tolerance, was_melee)
}

/// Same recovery, distance and repeated-shot choices for player bot and tactical NPC.
/// Permission to actually switch remains the controller's responsibility.
pub fn stance_choice(
    melee: bool,
    active: VitalityChannelId,
    pools: [u16; 2],
    maxima: [u16; 2],
    repeated_shots: bool,
    opponent: Option<(VitalityChannelId, [u16; 2])>,
) -> (VitalityChannelId, StanceReason) {
    let a = active.index();
    let b = active.other().index();
    // Recover only when the reserve is substantially healthier; cooldowns and
    // broken-pool gates remain in the real stance implementation.
    if u32::from(pools[a]) * 4 < u32::from(maxima[a].max(1))
        && u32::from(pools[b]) * 2 > u32::from(maxima[b].max(1))
    {
        return (active.other(), StanceReason::RecoverPool);
    }
    if repeated_shots {
        return (VitalityChannelId::One, StanceReason::PressAfterShots);
    }
    let _ = opponent; // Colour no longer dictates weapon family.
    (
        if melee {
            VitalityChannelId::One
        } else {
            VitalityChannelId::Two
        },
        StanceReason::Distance,
    )
}

/// Existing enemy light/heavy weighting and repetition cap, shared with the player bot.
/// Attack IDs are light=0, heavy=1, ranged=2.
pub fn attack_choice(ranged: bool, close: bool, last: u8, repeats: u8, roll: u16) -> u8 {
    if ranged {
        return 2;
    }
    if repeats >= 2 {
        return if last == 1 { 0 } else { 1 };
    }
    let mut weight = if close { 25 } else { 55 };
    if last == 1 {
        weight /= 3;
    } else if last == 0 {
        weight += 15;
    }
    if roll % 100 < weight {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spacing_has_hysteresis() {
        assert!(wants_melee(110, 100, 20, true));
        assert!(!wants_melee(110, 100, 20, false));
        assert!(wants_melee(100, 100, 20, false));
    }
    #[test]
    fn both_stances_and_health_recovery_have_reasons() {
        use VitalityChannelId::*;
        assert_eq!(
            stance_choice(false, One, [100, 100], [100, 100], false, None),
            (Two, StanceReason::Distance)
        );
        assert_eq!(
            stance_choice(true, Two, [100, 100], [100, 100], false, None),
            (One, StanceReason::Distance)
        );
        assert_eq!(
            stance_choice(true, One, [20, 90], [100, 100], false, None),
            (Two, StanceReason::RecoverPool)
        );
        assert_eq!(
            stance_choice(false, Two, [100, 100], [100, 100], true, None),
            (One, StanceReason::PressAfterShots)
        );
        assert_eq!(
            stance_choice(true, One, [20, 30], [100, 100], false, None).0,
            One
        );
    }
    #[test]
    fn guard_colour_does_not_override_spacing() {
        use VitalityChannelId::*;
        assert_eq!(stance_choice(true, One, [100,100],[100,100],false,Some((One,[100,100]))).0,One);
        assert_eq!(stance_choice(false, Two,[100,100],[100,100],false,Some((Two,[100,100]))).0,Two);
    }
    #[test]
    fn attacks_mix_without_an_endless_heavy_repeat() {
        assert_eq!(attack_choice(false, true, 1, 2, 0), 0);
        assert_eq!(attack_choice(false, true, 0, 2, 99), 1);
        assert_eq!(attack_choice(true, true, 0, 2, 99), 2);
        assert_ne!(
            attack_choice(false, false, 255, 0, 0),
            attack_choice(false, false, 255, 0, 99)
        );
    }
}
