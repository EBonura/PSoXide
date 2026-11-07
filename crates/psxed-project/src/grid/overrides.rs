//! Per-room wall policy: how walls come to exist, the room defaults they
//! fall back on, and the per-edge overrides that hold what the user changed
//! on a derived face.
//!
//! In [`WallMode::Authored`] (every room that predates derived walls) a wall
//! exists only where a `GridWall` record says so. In [`WallMode::Derived`] the
//! editor computes the vertical faces from the height differences between
//! neighbouring sectors (see [`super::derive`]); authored walls then only add
//! what heights cannot say. Overrides are addressed by geometry, never by an
//! index, so they survive edits to neighbouring cells and undo.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::face_uv::FaceUv;
use crate::ResourceId;

use super::paint_rules::PaintRules;

/// How a room's walls come to exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WallMode {
    /// Walls are exactly the `GridWall` records (legacy behaviour).
    #[default]
    Authored,
    /// Walls are derived from height differences; authored walls are
    /// additive.
    Derived,
}

impl WallMode {
    /// Whether this is the legacy default, for `skip_serializing_if`.
    pub fn is_authored(&self) -> bool {
        matches!(self, Self::Authored)
    }

    /// Lower-case label for the UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Authored => "authored",
            Self::Derived => "derived",
        }
    }
}

/// Axis a wall segment runs along.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Axis {
    /// Along +X (a north or south edge).
    X,
    /// Along +Z (an east or west edge).
    Z,
    /// Along the NW-SE diagonal of one sector, from its NW corner.
    NwSe,
    /// Along the NE-SW diagonal of one sector, from its NE corner.
    NeSw,
}

impl Axis {
    /// Whether this is one of the two diagonals.
    pub const fn is_diagonal(self) -> bool {
        matches!(self, Self::NwSe | Self::NeSw)
    }
}

/// Clicks in one sector edge, the length of every segment while the
/// document has whole-sector cells.
pub const SEGMENT_CLICKS: u8 = 4;

/// Geometry address of one wall segment: where it starts in world clicks
/// (256 grid units), the axis it runs along and its length in clicks. Both
/// sectors that share the edge produce the same key. A diagonal segment is
/// addressed by the sector's own origin (north-west corner) and the diagonal
/// axis; its length is the sector's edge length in clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeKey {
    /// World click of the segment start, X.
    pub x: i32,
    /// World click of the segment start, Z.
    pub z: i32,
    /// Direction the segment runs in.
    pub axis: Axis,
    /// Length in clicks.
    pub len: u8,
}

/// What the user changed on one derived segment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeOverride {
    /// No derived face here. Look only: closure walls keep their collision
    /// unless [`Self::solid`] says otherwise.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// Replaces the whole material chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<ResourceId>,
    /// Per-segment texture placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv: Option<FaceUv>,
    /// Collision. `None` is the default (closure walls block, risers are
    /// left to the step rule), `Some(true)` forces a solid wall record,
    /// `Some(false)` makes a ghost wall.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solid: Option<bool>,
    /// The band set of this segment: an index into the room's sets, or
    /// [`super::bands::NO_BANDS`] for none; `None` follows the room default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band_set: Option<u8>,
    /// Signed offset, grid units, added to the top of a closure wall that
    /// takes its height from the room's `open_wall` (a floor with no
    /// ceiling) at the two ends of the segment, in the key's direction: the
    /// crumbled top of an open-topped rock wall. `[0, 0]` is no change.
    #[serde(default, skip_serializing_if = "is_zero_pair")]
    pub top_delta: [i16; 2],
}

fn is_zero_pair(value: &[i16; 2]) -> bool {
    *value == [0, 0]
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl EdgeOverride {
    /// Whether the override changes nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The overrides of a room, ordered by key so files are deterministic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EdgeOverrides(BTreeMap<EdgeKey, EdgeOverride>);

impl EdgeOverrides {
    /// Override of `key`, if any.
    pub fn get(&self, key: &EdgeKey) -> Option<&EdgeOverride> {
        self.0.get(key)
    }

    /// Edit the override of `key`, creating it when missing and dropping it
    /// again when the edit leaves it empty.
    pub fn edit(&mut self, key: EdgeKey, change: impl FnOnce(&mut EdgeOverride)) {
        let entry = self.0.entry(key).or_default();
        change(entry);
        if entry.is_empty() {
            self.0.remove(&key);
        }
    }

    /// Remove the override of `key`.
    pub fn remove(&mut self, key: &EdgeKey) -> Option<EdgeOverride> {
        self.0.remove(key)
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of overrides.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Every override in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&EdgeKey, &EdgeOverride)> {
        self.0.iter()
    }

    /// Renumber the band sets the overrides name: `map` gets each index and
    /// returns the new one (`None` drops the assignment). Overrides this
    /// leaves empty are removed.
    pub fn map_band_sets(&mut self, map: impl Fn(u8) -> Option<u8>) {
        for o in self.0.values_mut() {
            o.band_set = o.band_set.and_then(&map);
        }
        self.0.retain(|_, o| !o.is_empty());
    }

    /// Keep only the overrides `keep` accepts.
    pub fn retain(&mut self, mut keep: impl FnMut(&EdgeKey) -> bool) {
        self.0.retain(|k, _| keep(k));
    }
}

impl Serialize for EdgeOverrides {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter())
    }
}

impl<'de> Deserialize<'de> for EdgeOverrides {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pairs = Vec::<(EdgeKey, EdgeOverride)>::deserialize(deserializer)?;
        Ok(Self(pairs.into_iter().collect()))
    }
}

/// Which sides of a derived closure wall are drawn.
///
/// A closure wall stands where the room ends. Drawn from the inside only
/// (the default, and what every level made before this setting has), a
/// building seen from outside is hollow: the far inside walls show through
/// the missing near ones. `Both` draws the closure walls from the outside too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClosureFacing {
    /// Seen from inside the room only.
    #[default]
    Front,
    /// Seen from inside and from outside.
    Both,
}

impl ClosureFacing {
    /// Whether this is the default, for `skip_serializing_if`.
    pub fn is_front(&self) -> bool {
        matches!(self, Self::Front)
    }

    /// Lower-case label for the UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Front => "inside only",
            Self::Both => "inside and outside",
        }
    }
}

/// Room-wide fallbacks for derived faces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomDefaults {
    /// Wall material when nothing more specific is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_material: Option<ResourceId>,
    /// The band set every wall of the room uses unless it names another (or
    /// none): an index into the room's sets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<u8>,
    /// Height of the closure wall around a floor that has no ceiling, grid
    /// units. 0 leaves such edges open (the legacy outdoor look).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub open_wall: i32,
    /// What the level verbs paint when they shape the ground.
    #[serde(default, skip_serializing_if = "PaintRules::is_default")]
    pub rules: PaintRules,
    /// Which sides of the closure walls are drawn: a building that is seen
    /// from outside wants `Both`. Collision is unchanged.
    #[serde(default, skip_serializing_if = "ClosureFacing::is_front")]
    pub closure_facing: ClosureFacing,
}

fn is_zero(value: &i32) -> bool {
    *value == 0
}

impl RoomDefaults {
    /// Whether every field is at its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(x: i32) -> EdgeKey {
        EdgeKey {
            x,
            z: 0,
            axis: Axis::X,
            len: SEGMENT_CLICKS,
        }
    }

    #[test]
    fn overrides_round_trip_in_key_order_and_empty_ones_vanish() {
        let mut map = EdgeOverrides::default();
        map.edit(key(8), |o| o.hidden = true);
        map.edit(key(4), |o| o.solid = Some(false));
        map.edit(key(12), |o| o.hidden = true);
        map.edit(key(12), |o| o.hidden = false);
        assert_eq!(map.len(), 2);
        let text = ron::to_string(&map).unwrap();
        assert!(
            text.find("x:4").unwrap() < text.find("x:8").unwrap(),
            "{text}"
        );
        let back: EdgeOverrides = ron::from_str(&text).unwrap();
        assert_eq!(back, map);
    }

    #[test]
    fn defaults_stay_out_of_the_file() {
        let text = ron::to_string(&EdgeOverride::default()).unwrap();
        assert_eq!(text, "()");
        assert!(RoomDefaults::default().is_default());
        assert!(WallMode::default().is_authored());
    }
}
