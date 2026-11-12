//! What the player is able to do: the abilities a level starts with and the
//! ones earned from pickups.
//!
//! Abilities are bits of a `u16` (`psx_level::ability`). A level declares the
//! ones the player starts with; a point of interest reward grants more and the
//! save block keeps those. What the player can do now is the union of the two,
//! so a save never has to remember a level's starting set and re-authoring a
//! level's starting set takes effect on old saves.

use psx_level::ability;

/// A set of player abilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerAbilities(u16);

impl PlayerAbilities {
    /// No ability.
    pub const NONE: Self = Self(ability::NONE);
    /// Every ability this build knows: what a level that authors nothing
    /// starts with.
    pub const ALL: Self = Self(ability::ALL);

    /// The set of `bits`. Bits no ability owns are dropped, so a record from
    /// a newer cook can never turn on something this build does not have.
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits & ability::ALL)
    }

    /// The bits of the set.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Every ability in `self` or `other`.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every ability in `bits` is in the set.
    pub const fn has(self, bits: u16) -> bool {
        self.0 & bits == bits
    }

    /// Whether Circle held sprints.
    pub const fn sprint(self) -> bool {
        self.has(ability::SPRINT)
    }

    /// Whether a Circle tap dashes.
    pub const fn dash(self) -> bool {
        self.has(ability::DASH)
    }

    /// What a player starting with `start` and having earned `earned` can do.
    pub const fn effective(start: u16, earned: u16) -> Self {
        Self::from_bits(start).union(Self::from_bits(earned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_that_authors_nothing_leaves_everything_unlocked() {
        assert_eq!(PlayerAbilities::ALL.bits(), ability::SPRINT | ability::DASH);
        assert!(PlayerAbilities::ALL.sprint() && PlayerAbilities::ALL.dash());
        assert!(!PlayerAbilities::NONE.sprint() && !PlayerAbilities::NONE.dash());
    }

    #[test]
    fn earned_abilities_add_to_the_levels_starting_set() {
        let none = PlayerAbilities::effective(ability::NONE, ability::NONE);
        assert!(!none.sprint() && !none.dash());
        let sprint = PlayerAbilities::effective(ability::NONE, ability::SPRINT);
        assert!(sprint.sprint() && !sprint.dash());
        let both = PlayerAbilities::effective(ability::DASH, ability::SPRINT);
        assert!(both.sprint() && both.dash());
        // Granting twice is the same as once.
        assert_eq!(
            PlayerAbilities::effective(ability::SPRINT, ability::SPRINT).bits(),
            ability::SPRINT
        );
    }

    #[test]
    fn unknown_bits_never_turn_anything_on() {
        assert_eq!(PlayerAbilities::from_bits(0xFFFF), PlayerAbilities::ALL);
        assert_eq!(PlayerAbilities::from_bits(1 << 9), PlayerAbilities::NONE);
    }
}
