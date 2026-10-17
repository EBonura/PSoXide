//! Smart paint rules: what the level verbs paint when they shape the ground.
//!
//! Rules are optional and live on the room ([`super::RoomDefaults::rules`]).
//! They run **only inside verbs**, and only on a surface the verb created or
//! whose slope class the verb changed; a surface painted by hand
//! ([`super::GridSurface::paint_locked`]) is skipped. Everything a rule writes
//! is an ordinary material field, so it stays editable by hand afterwards.

use serde::{Deserialize, Serialize};

use crate::ResourceId;

use super::slope::SlopeClass;

/// The rules of one room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaintRules {
    /// Floor material by slope class: flat, walkable slope, too steep.
    #[serde(default)]
    pub floor_by_class: [Option<ResourceId>; 3],
    /// Material of band 0 (the wall's foot) on every wall of a room that has
    /// no default band set, written when a verb makes a derived face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim: Option<ResourceId>,
    /// Material a verb gives the derived walls of the sectors it shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall: Option<ResourceId>,
}

impl PaintRules {
    /// Whether no rule is set, for `skip_serializing_if`.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The floor material for `class`, if a rule names one.
    pub fn floor_for(&self, class: SlopeClass) -> Option<ResourceId> {
        self.floor_by_class[class.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_rules_stay_out_of_the_file() {
        assert!(PaintRules::default().is_default());
        let rules = PaintRules {
            trim: None,
            ..PaintRules::default()
        };
        assert!(rules.is_default());
        assert!(rules.floor_for(SlopeClass::Steep).is_none());
    }
}
