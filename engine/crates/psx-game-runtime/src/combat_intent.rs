//! What a controller asks a fighter to do: the verb list as requests.
//!
//! Player and enemies share one rule set and the AI is only a controller.
//! A controller (the pad, an enemy's tactics, the duel bot) says what it
//! wants with [`CombatIntent`]s; the fighter's rules decide whether and how
//! each one happens. Whether a fighter may perform an intent at all is its
//! kits ([`CombatKits`], [`CombatIntent::permitted`]): no blade, no strike;
//! no cannon, no shot; no mobility kit, no dodge; not both weapons, no swap.
//!
//! The parry ripostes are not separate requests: a light strike or a shot
//! made while a crit is armed is the crit strike or crit shot.
//!
//! Nothing produces or consumes intents yet; wiring the enemy AI and the pad
//! through them is the next step of the controller seam.

use psx_level::combat_verbs::{CombatKit, CombatKits};

/// One request from a controller for one simulation tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatIntent {
    /// Stand still.
    Idle,
    /// Move along a stick vector in Q12 (`x` right, `z` forward, each
    /// -4096..=4096), running when `run` is set. While aiming this is the
    /// ranged walk.
    Move {
        /// Q12 sideways component.
        x: i16,
        /// Q12 forward component.
        z: i16,
        /// Run instead of walk.
        run: bool,
    },
    /// Turn in place to face `yaw` (PSX angle units, 4096 per turn).
    Turn {
        /// Target facing.
        yaw: i16,
    },
    /// Blade light strike (the crit strike while a crit is armed).
    LightAttack,
    /// Blade heavy strike.
    HeavyAttack,
    /// Raise the cannon and hold the aim.
    Aim,
    /// Fire the cannon (the crit shot while a crit is armed).
    Shoot,
    /// Swap stance: blade to Zenith or back.
    Swap,
    /// Forward evade.
    Roll,
    /// Backward evade.
    Backstep,
    /// Locked-on evade to the left.
    DashLeft,
    /// Locked-on evade to the right.
    DashRight,
}

impl CombatIntent {
    /// The kit a fighter needs to perform this intent.
    pub const fn kit(self) -> CombatKit {
        match self {
            Self::Idle | Self::Move { .. } | Self::Turn { .. } => CombatKit::Core,
            Self::LightAttack | Self::HeavyAttack => CombatKit::Melee,
            Self::Aim | Self::Shoot => CombatKit::Ranged,
            Self::Swap => CombatKit::Swap,
            Self::Roll | Self::Backstep | Self::DashLeft | Self::DashRight => CombatKit::Mobility,
        }
    }

    /// Whether a fighter with `kits` may perform this intent at all.
    pub const fn permitted(self, kits: CombatKits) -> bool {
        kits.has(self.kit())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kits_permit_exactly_their_intents() {
        let melee = CombatKits::from_bits(CombatKits::MELEE);
        let ranged = CombatKits::from_bits(CombatKits::RANGED);
        let full =
            CombatKits::from_bits(CombatKits::MELEE | CombatKits::RANGED | CombatKits::MOBILITY);
        let mover = CombatIntent::Move {
            x: 0,
            z: 4096,
            run: true,
        };
        for kits in [CombatKits::NONE, melee, ranged, full] {
            assert!(CombatIntent::Idle.permitted(kits) && mover.permitted(kits));
        }
        assert!(CombatIntent::LightAttack.permitted(melee));
        assert!(!CombatIntent::Shoot.permitted(melee));
        assert!(!CombatIntent::Swap.permitted(melee));
        assert!(!CombatIntent::Roll.permitted(melee));
        assert!(
            CombatIntent::Aim.permitted(ranged) && !CombatIntent::HeavyAttack.permitted(ranged)
        );
        assert!(CombatIntent::Swap.permitted(full) && CombatIntent::DashLeft.permitted(full));
    }
}
