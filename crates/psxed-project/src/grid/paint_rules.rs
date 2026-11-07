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

/// A material per height band: up to three thresholds split the heights into
/// up to four bands, low to high (scree below, rock above, snow on top).
///
/// Thresholds are in 64 grid unit steps relative to the room's base (16 bits
/// is plenty: +-2048 sectors); a band with no material leaves what is there.
/// The number of bands is the last band that names a material, so unused
/// thresholds cost nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeightRule {
    /// Where band 1, 2 and 3 begin, ascending, in 64 grid unit steps.
    #[serde(default)]
    pub at: [i16; 3],
    /// The material of band 0 (below `at[0]`) to 3.
    #[serde(default)]
    pub materials: [Option<ResourceId>; 4],
}

impl HeightRule {
    /// Whether the rule names no material.
    pub fn is_empty(&self) -> bool {
        self.materials.iter().all(Option::is_none)
    }

    /// How many bands the rule has (0 when empty).
    pub fn bands(&self) -> usize {
        self.materials
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |i| i + 1)
    }

    /// The band a room-relative height, grid units, falls in.
    pub fn band_of(&self, height: i32) -> usize {
        (0..self.bands().saturating_sub(1))
            .take_while(|&i| height >= i32::from(self.at[i]) * 64)
            .count()
    }

    /// The material for `height`, if its band names one.
    pub fn material_at(&self, height: i32) -> Option<ResourceId> {
        if self.is_empty() {
            return None;
        }
        self.materials[self.band_of(height)]
    }

    /// The thresholds in use, as room-relative grid units.
    pub fn lines(&self) -> Vec<i32> {
        (0..self.bands().saturating_sub(1))
            .map(|i| i32::from(self.at[i]) * 64)
            .collect()
    }
}

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
    /// Floor material by height: scree low, rock high. A steep floor keeps
    /// its slope-class material when that rule names one.
    #[serde(default, skip_serializing_if = "HeightRule::is_empty")]
    pub floor_by_height: HeightRule,
    /// Wall material by height: strata. The verbs make it the room's default
    /// band set, with an absolute cut line at each threshold.
    #[serde(default, skip_serializing_if = "HeightRule::is_empty")]
    pub wall_by_height: HeightRule,
}

impl PaintRules {
    /// Whether no rule is set, for `skip_serializing_if`.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The floor material a floor of slope `class` whose middle is at
    /// `height` (room-relative grid units) gets: the height rule, unless the
    /// floor is steep and the class rule names one for it.
    pub fn floor_for_height(&self, class: SlopeClass, height: i32) -> Option<ResourceId> {
        if class == SlopeClass::Steep && self.floor_for(class).is_some() {
            return None;
        }
        self.floor_by_height.material_at(height)
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
    fn a_height_rule_picks_its_band_and_unused_thresholds_cost_nothing() {
        let mut rid = |n: u64| -> ResourceId { ron::from_str(&format!("({n})")).unwrap() };
        let rule = HeightRule {
            at: [4, 16, 0],
            materials: [Some(rid(1)), Some(rid(2)), Some(rid(3)), None],
        };
        assert_eq!(rule.bands(), 3);
        assert_eq!(rule.lines(), vec![256, 1024]);
        assert_eq!(rule.material_at(-100), Some(rid(1)));
        assert_eq!(rule.material_at(255), Some(rid(1)));
        assert_eq!(rule.material_at(256), Some(rid(2)));
        assert_eq!(rule.material_at(5000), Some(rid(3)));
        assert!(HeightRule::default().is_empty());
        assert_eq!(HeightRule::default().material_at(0), None);
        let rules = PaintRules {
            floor_by_height: rule,
            floor_by_class: [None, None, Some(rid(9))],
            ..PaintRules::default()
        };
        assert_eq!(rules.floor_for_height(SlopeClass::Flat, 300), Some(rid(2)));
        assert_eq!(rules.floor_for_height(SlopeClass::Steep, 300), None);
        let text = ron::to_string(&rules).unwrap();
        assert_eq!(ron::from_str::<PaintRules>(&text).unwrap(), rules);
        let _ = &mut rid;
    }

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
