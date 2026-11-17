//! What a controller asks a fighter to do: the verb list as requests.
//!
//! Player and enemies share one rule set and the AI is only a controller.
//! A controller (the pad, an enemy's tactics, the duel bot) says what it
//! wants with one [`CombatIntent`] per tick; the fighter's rules decide
//! whether and how each request happens. Whether a fighter may perform an
//! action at all is its kits ([`CombatKits`], [`CombatAction::permitted`]):
//! no blade, no strike; no cannon, no shot; no mobility kit, no dodge; not
//! both weapons, no swap.
//!
//! The parry ripostes are not separate requests: a light strike or a shot
//! made while a crit is armed is the crit strike or crit shot.
//!
//! Actions come as edges and holds, the way a pad reports them, so a
//! controller that presses (the pad) and one that decides (an AI) speak the
//! same language: a strike is a press, aiming is a hold, and a tap of the
//! evade button is a press followed by a release.

use psx_level::combat_verbs::{CombatKit, CombatKits};

/// One action a controller can request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CombatAction {
    /// Blade light strike (the crit strike while a crit is armed).
    LightAttack = 0,
    /// Blade heavy strike.
    HeavyAttack = 1,
    /// Raise the cannon and hold the aim.
    Aim = 2,
    /// Fire the cannon (the crit shot while a crit is armed).
    Shoot = 3,
    /// Swap stance: blade to Zenith or back.
    Swap = 4,
    /// Evade. The mover picks the roll, backstep or locked-on dash from the
    /// movement and the lock; a pad's evade button also runs while held.
    Evade = 5,
    /// Toggle the lock on the nearest target.
    Lock = 6,
}

impl CombatAction {
    /// Every action, in bit order.
    pub const ALL: [Self; 7] = [
        Self::LightAttack,
        Self::HeavyAttack,
        Self::Aim,
        Self::Shoot,
        Self::Swap,
        Self::Evade,
        Self::Lock,
    ];

    /// The kit a fighter needs to perform this action.
    pub const fn kit(self) -> CombatKit {
        match self {
            Self::Lock => CombatKit::Core,
            Self::LightAttack | Self::HeavyAttack => CombatKit::Melee,
            Self::Aim | Self::Shoot => CombatKit::Ranged,
            Self::Swap => CombatKit::Swap,
            Self::Evade => CombatKit::Mobility,
        }
    }

    /// Whether a fighter with `kits` may perform this action at all.
    pub const fn permitted(self, kits: CombatKits) -> bool {
        kits.has(self.kit())
    }

    /// The strike or shot the shared attack policy names by its id
    /// (light 0, heavy 1, ranged 2, as
    /// [`attack_choice`](crate::combat_policy::attack_choice) returns them).
    pub const fn from_attack_id(id: u8) -> Self {
        match id {
            0 => Self::LightAttack,
            1 => Self::HeavyAttack,
            _ => Self::Shoot,
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of [`CombatAction`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CombatActions(u8);

impl CombatActions {
    /// No action.
    pub const NONE: Self = Self(0);

    /// The set holding exactly `action`.
    pub const fn of(action: CombatAction) -> Self {
        Self(action.bit())
    }

    /// This set plus `action`.
    #[must_use]
    pub const fn with(self, action: CombatAction) -> Self {
        Self(self.0 | action.bit())
    }

    /// This set plus `action` when `on`.
    #[must_use]
    pub const fn with_if(self, action: CombatAction, on: bool) -> Self {
        if on {
            self.with(action)
        } else {
            self
        }
    }

    /// Whether `action` is in the set.
    pub const fn has(self, action: CombatAction) -> bool {
        self.0 & action.bit() != 0
    }

    /// Whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The actions of this set a fighter with `kits` may perform.
    #[must_use]
    pub fn permitted(self, kits: CombatKits) -> Self {
        let mut kept = Self::NONE;
        for action in CombatAction::ALL {
            kept = kept.with_if(action, self.has(action) && action.permitted(kits));
        }
        kept
    }
}

/// Where a controller wants the fighter to go this tick: one form for every
/// controller.
///
/// A pad turns its stick into this through the camera; an AI writes it
/// from where it wants to be. The fighter's motor (the same one for every
/// character) reads only this, so it cannot tell who asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CombatMove {
    /// Stand still.
    #[default]
    Still,
    /// Move along a world direction (`x`, `z`, Q12 with 4096 for full
    /// strength: full walking pace, or running pace while running).
    Move {
        /// World x component, Q12.
        x: i16,
        /// World z component, Q12.
        z: i16,
        /// The controller's own forward/back sense: +1, -1 or 0. A pad
        /// gives its stick's forward axis; the motor walks along the body's
        /// facing on it only when the vector has been scaled to nothing
        /// (a walk-start ramp). An AI leaves it 0.
        forward: i8,
    },
}

/// One request from a controller for one simulation tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CombatIntent {
    /// Where to go.
    pub movement: CombatMove,
    /// A world point to turn toward, at the fighter's own turn rate. None
    /// keeps the facing the movement and the rules give.
    pub face: Option<[i32; 3]>,
    /// The world point a shot requested this tick aims at. None lets the
    /// fighter's own aim decide (the pad's reticle).
    pub aim: Option<[i32; 3]>,
    /// Actions requested this tick (press edges).
    pub pressed: CombatActions,
    /// Actions held this tick.
    pub held: CombatActions,
    /// Actions let go this tick (release edges).
    pub released: CombatActions,
}

impl CombatIntent {
    /// Stand still and do nothing.
    pub const IDLE: Self = Self {
        movement: CombatMove::Still,
        face: None,
        aim: None,
        pressed: CombatActions::NONE,
        held: CombatActions::NONE,
        released: CombatActions::NONE,
    };

    /// Whether `action` was pressed this tick.
    pub const fn pressed(&self, action: CombatAction) -> bool {
        self.pressed.has(action)
    }

    /// Whether `action` is held this tick.
    pub const fn held(&self, action: CombatAction) -> bool {
        self.held.has(action)
    }

    /// Whether `action` was let go this tick.
    pub const fn released(&self, action: CombatAction) -> bool {
        self.released.has(action)
    }

    /// The strike or shot pressed this tick, if any: the light strike
    /// first, then the heavy, then the shot.
    pub const fn attack(&self) -> Option<CombatAction> {
        if self.pressed(CombatAction::LightAttack) {
            Some(CombatAction::LightAttack)
        } else if self.pressed(CombatAction::HeavyAttack) {
            Some(CombatAction::HeavyAttack)
        } else if self.pressed(CombatAction::Shoot) {
            Some(CombatAction::Shoot)
        } else {
            None
        }
    }

    /// This intent with only the actions a fighter with `kits` may
    /// perform. Movement and facing need no kit.
    #[must_use]
    pub fn permitted(self, kits: CombatKits) -> Self {
        Self {
            pressed: self.pressed.permitted(kits),
            held: self.held.permitted(kits),
            released: self.released.permitted(kits),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kits_permit_exactly_their_actions() {
        let melee = CombatKits::from_bits(CombatKits::MELEE);
        let ranged = CombatKits::from_bits(CombatKits::RANGED);
        let full =
            CombatKits::from_bits(CombatKits::MELEE | CombatKits::RANGED | CombatKits::MOBILITY);
        for kits in [CombatKits::NONE, melee, ranged, full] {
            assert!(CombatAction::Lock.permitted(kits));
        }
        assert!(CombatAction::LightAttack.permitted(melee));
        assert!(!CombatAction::Shoot.permitted(melee));
        assert!(!CombatAction::Swap.permitted(melee));
        assert!(!CombatAction::Evade.permitted(melee));
        assert!(
            CombatAction::Aim.permitted(ranged) && !CombatAction::HeavyAttack.permitted(ranged)
        );
        assert!(CombatAction::Swap.permitted(full) && CombatAction::Evade.permitted(full));
    }

    #[test]
    fn an_intent_keeps_only_what_the_kits_allow() {
        let every = CombatAction::ALL
            .iter()
            .fold(CombatActions::NONE, |set, &action| set.with(action));
        let intent = CombatIntent {
            movement: CombatMove::Move {
                x: 1229,
                z: -1638,
                forward: 0,
            },
            face: Some([1, 2, 3]),
            aim: None,
            pressed: every,
            held: every,
            released: every,
        };
        let melee = intent.permitted(CombatKits::from_bits(CombatKits::MELEE));
        assert_eq!(melee.movement, intent.movement);
        assert_eq!(melee.face, intent.face);
        for set in [melee.pressed, melee.held, melee.released] {
            assert!(set.has(CombatAction::LightAttack) && set.has(CombatAction::HeavyAttack));
            assert!(set.has(CombatAction::Lock));
            assert!(!set.has(CombatAction::Shoot) && !set.has(CombatAction::Swap));
            assert!(!set.has(CombatAction::Evade) && !set.has(CombatAction::Aim));
        }
        assert_eq!(melee.attack(), Some(CombatAction::LightAttack));
        assert_eq!(CombatIntent::IDLE.attack(), None);
        assert_eq!(
            CombatIntent {
                pressed: CombatActions::of(CombatAction::Shoot),
                ..CombatIntent::IDLE
            }
            .attack(),
            Some(CombatAction::Shoot)
        );
    }

    #[test]
    fn attack_ids_name_the_policy_attacks() {
        assert_eq!(CombatAction::from_attack_id(0), CombatAction::LightAttack);
        assert_eq!(CombatAction::from_attack_id(1), CombatAction::HeavyAttack);
        assert_eq!(CombatAction::from_attack_id(2), CombatAction::Shoot);
    }
}
