//! Shared tactical choices. Controllers execute intents through their own legal actions.
use crate::combat_intent::CombatAction;
use crate::vitality::VitalityChannelId;

/// Why the policy prefers a stance. Stable values are used by duel reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StanceReason {
    /// Match the current separation with hysteresis.
    Distance = 0,
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

/// Same distance and repeated-shot choices for player bot and tactical NPC.
/// Permission to actually switch remains the controller's responsibility.
///
/// There is no health reason to swap: the one health bar does not depend on
/// the stance. (Value 1, the old "recover pool" reason, is retired.)
pub fn stance_choice(
    melee: bool,
    repeated_shots: bool,
    opponent: Option<VitalityChannelId>,
) -> (VitalityChannelId, StanceReason) {
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

/// Whether to swap on purpose to parry: a hit seen `ticks_to_hit` ticks away
/// has just entered the fighter's parry window of `parry_ticks` (it was not
/// inside it at the previous check, `before`), the fighter may swap now,
/// and its roll falls under `percent` (its
/// [`CombatStats::parry_swap_percent`](psx_level::CombatStats::parry_swap_percent)).
/// Deciding on entry gives one roll per incoming hit at any check cadence,
/// and swaps at the earliest tick that still parries. A hit `ticks_to_hit`
/// away is inside the window when `ticks_to_hit < parry_ticks`: the swap clock
/// advances once on the swap's own tick, so the hit lands with the swap
/// `ticks_to_hit + 1` ticks old, and the window covers `parry_ticks` of them. `roll` (0..100) is
/// drawn only when everything else holds, so a fighter that never sees a
/// parryable hit keeps its random sequence. The same choice for the duel's
/// player bot and every tactical enemy; what each can see of the incoming
/// hit is the controller's own perception.
///
/// Reaction: the fighter must have seen the hit coming (`watched` ticks ago,
/// counted from the first decision that saw it) at least `reaction_ticks`
/// before it lands, `watched + ticks_to_hit >= reaction_ticks` (its
/// [`CombatStats::parry_reaction_ticks`](psx_level::CombatStats::parry_reaction_ticks)),
/// so a deliberate parry follows a readable tell. A hit first seen too late
/// is not parried on purpose; it is still parried if it happens to land in a
/// swap's window.
#[allow(clippy::too_many_arguments)]
pub fn parry_swap(
    ticks_to_hit: Option<u16>,
    before: Option<u16>,
    watched: u16,
    parry_ticks: u16,
    reaction_ticks: u16,
    can_swap: bool,
    percent: u8,
    roll: impl FnOnce() -> u16,
) -> bool {
    let inside = |ticks: Option<u16>| ticks.is_some_and(|ticks| ticks < parry_ticks);
    let readable = |ticks: Option<u16>| {
        ticks
            .is_some_and(|ticks| u32::from(watched) + u32::from(ticks) >= u32::from(reaction_ticks))
    };
    can_swap
        && percent != 0
        && parry_ticks != 0
        && inside(ticks_to_hit)
        && !inside(before)
        && readable(ticks_to_hit)
        && roll() % 100 < u16::from(percent)
}

/// What a fighter does with the crit its parry armed: attack with the
/// weapon the parry swapped to. In the blade stance that is the light
/// strike (the crit strike) once the target is in reach; in the Zenith
/// stance the shot (the crit shot) when the fighter can afford it. None
/// with no crit armed, or when the weapon cannot reach yet.
pub fn crit_followup(
    crit_armed: bool,
    stance: VitalityChannelId,
    in_melee_reach: bool,
    can_shoot: bool,
) -> Option<CombatAction> {
    if !crit_armed {
        return None;
    }
    match stance {
        VitalityChannelId::One => in_melee_reach.then_some(CombatAction::LightAttack),
        VitalityChannelId::Two => can_shoot.then_some(CombatAction::Shoot),
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
    fn both_stances_have_reasons_and_health_is_not_one() {
        use VitalityChannelId::*;
        assert_eq!(
            stance_choice(false, false, None),
            (Two, StanceReason::Distance)
        );
        assert_eq!(
            stance_choice(true, false, None),
            (One, StanceReason::Distance)
        );
        assert_eq!(
            stance_choice(false, true, None),
            (One, StanceReason::PressAfterShots)
        );
    }
    #[test]
    fn guard_colour_does_not_override_spacing() {
        use VitalityChannelId::*;
        assert_eq!(stance_choice(true, false, Some(One)).0, One);
        assert_eq!(stance_choice(false, false, Some(Two)).0, Two);
    }
    #[test]
    fn a_parry_swap_needs_a_hit_inside_the_window_and_a_roll() {
        let yes = || 10;
        let no = || 90;
        assert!(parry_swap(Some(3), None, 0, 8, 0, true, 50, yes));
        assert!(parry_swap(Some(7), Some(8), 0, 8, 0, true, 50, yes));
        // Eight ticks away lands nine ticks into the swap: past the window.
        assert!(!parry_swap(Some(8), None, 0, 8, 0, true, 50, yes));
        assert!(!parry_swap(Some(9), None, 0, 8, 0, true, 50, yes));
        assert!(!parry_swap(None, Some(9), 0, 8, 0, true, 50, yes));
        assert!(!parry_swap(Some(3), None, 0, 8, 0, false, 50, yes));
        assert!(!parry_swap(Some(3), None, 0, 8, 0, true, 0, yes));
        assert!(!parry_swap(Some(3), None, 0, 0, 0, true, 50, yes));
        assert!(!parry_swap(Some(3), None, 0, 8, 0, true, 50, no));
        // One roll per hit: already inside the window at the last check.
        assert!(!parry_swap(Some(3), Some(4), 0, 8, 0, true, 50, yes));
        // No draw unless everything else holds.
        let mut drawn = false;
        assert!(!parry_swap(Some(20), None, 0, 8, 0, true, 50, || {
            drawn = true;
            0
        }));
        assert!(!drawn);
    }
    #[test]
    fn a_parry_swap_needs_the_hit_seen_its_reaction_time_before_contact() {
        let yes = || 10;
        // Seen 12 ticks before contact (watched 9, lands in 3): reaction 12 holds.
        assert!(parry_swap(Some(3), None, 9, 8, 12, true, 50, yes));
        // Seen 11 ticks before contact: too late for a 12-tick reaction.
        assert!(!parry_swap(Some(3), None, 8, 8, 12, true, 50, yes));
        // First seen already inside the window: no deliberate parry.
        assert!(!parry_swap(Some(3), None, 0, 8, 12, true, 50, yes));
        // No draw when the hit was seen too late.
        let mut drawn = false;
        assert!(!parry_swap(Some(3), None, 0, 8, 12, true, 50, || {
            drawn = true;
            0
        }));
        assert!(!drawn);
        // Zero reaction: the old rule.
        assert!(parry_swap(Some(3), None, 0, 8, 0, true, 50, yes));
    }
    #[test]
    fn a_crit_is_spent_with_the_swapped_to_weapon() {
        use VitalityChannelId::*;
        assert_eq!(
            crit_followup(true, One, true, true),
            Some(CombatAction::LightAttack)
        );
        assert_eq!(crit_followup(true, One, false, true), None);
        assert_eq!(
            crit_followup(true, Two, false, true),
            Some(CombatAction::Shoot)
        );
        assert_eq!(crit_followup(true, Two, true, false), None);
        assert_eq!(crit_followup(false, One, true, true), None);
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
