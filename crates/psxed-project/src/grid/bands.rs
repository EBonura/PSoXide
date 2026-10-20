//! Horizontal wall bands: a wall run cut by horizontal lines into strips,
//! each with its own material and texture rule (a trim at the base, the main
//! body, a frieze at the top).
//!
//! A [`BandSet`] is an ordered list of cut lines and one more band than
//! lines: band 0 runs from the wall's bottom to line 0, the last band from
//! the last line to the wall's top. A line is an [`Anchor`]: a height above
//! the wall's bottom edge, below its top edge, or an absolute room height.
//! Sets live on the room; a room names a default set, and a derived wall's
//! override or an authored wall can point at another (or at none). Nothing
//! is baked into faces in the document: `faces` cuts the walls when it builds
//! them (see [`super::band_geom`]).

use serde::{Deserialize, Serialize};

use crate::face_uv::FaceUv;
use crate::ResourceId;

use super::room::GridRoom;

/// The set index that means "no bands on this wall", even in a room whose
/// default set would apply.
pub const NO_BANDS: u8 = 255;
/// Most cut lines one set holds (seven bands).
pub const MAX_BAND_LINES: usize = 6;
/// Most sets one room holds.
pub const MAX_BAND_SETS: usize = 32;

/// Where a cut line sits, in grid units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anchor {
    /// Above the wall's bottom edge, so a trim follows a sloped floor.
    Bottom(i32),
    /// Below the wall's top edge, so a frieze hangs from a ceiling or ledge.
    Top(i32),
    /// An absolute room-relative height: a datum line across terrain.
    Abs(i32),
}

impl Anchor {
    /// The anchor's offset or height.
    pub const fn value(self) -> i32 {
        match self {
            Self::Bottom(v) | Self::Top(v) | Self::Abs(v) => v,
        }
    }

    /// The same kind of anchor with another value.
    pub const fn with_value(self, value: i32) -> Self {
        match self {
            Self::Bottom(_) => Self::Bottom(value),
            Self::Top(_) => Self::Top(value),
            Self::Abs(_) => Self::Abs(value),
        }
    }

    /// Lower-case label of the kind.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bottom(_) => "bottom",
            Self::Top(_) => "top",
            Self::Abs(_) => "abs",
        }
    }
}

/// Where V = 0 sits for a band's texture when the band maps in world mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BandV {
    /// World aligned: V follows the world height, as on a plain wall.
    #[default]
    World,
    /// The texture's top row on the band's lower line, so a trim sits on the
    /// floor whatever the floor's height or slope.
    FromBottom,
    /// The texture's top row on the band's upper line, so a frieze hangs from
    /// the top.
    FromTop,
}

impl BandV {
    /// Lower-case label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::FromBottom => "from bottom",
            Self::FromTop => "from top",
        }
    }
}

fn identity(uv: &FaceUv) -> bool {
    uv.is_identity()
}

fn world_v(v: &BandV) -> bool {
    *v == BandV::World
}

/// One strip of a banded wall.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Band {
    /// The strip's material; `None` keeps the wall's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<ResourceId>,
    /// How the texture is laid over the strip. A fit mapping repeats across
    /// the strip's run and fits exactly between its two lines.
    #[serde(default, skip_serializing_if = "identity")]
    pub uv: FaceUv,
    /// Where V = 0 sits in a world mapping.
    #[serde(default, skip_serializing_if = "world_v")]
    pub v: BandV,
}

/// Ordered cut lines and the bands between them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandSet {
    /// Name shown in the Rooms panel.
    #[serde(default)]
    pub name: String,
    /// Cut lines from the bottom up.
    #[serde(default)]
    pub lines: Vec<Anchor>,
    /// One band more than there are lines, from the bottom up.
    pub bands: Vec<Band>,
}

impl BandSet {
    /// A set of one band and no lines.
    pub fn single(name: impl Into<String>, material: Option<ResourceId>) -> Self {
        Self {
            name: name.into(),
            lines: Vec::new(),
            bands: vec![Band {
                material,
                ..Band::default()
            }],
        }
    }

    /// Whether the set is well formed: one more band than lines, at most
    /// [`MAX_BAND_LINES`] lines.
    pub fn is_valid(&self) -> bool {
        self.bands.len() == self.lines.len() + 1 && self.lines.len() <= MAX_BAND_LINES
    }

    /// Whether the set changes anything (a lone band is just the wall).
    pub fn is_plain(&self) -> bool {
        self.lines.is_empty()
            && self
                .bands
                .first()
                .is_none_or(|b| b.material.is_none() && b.uv.is_identity() && b.v == BandV::World)
    }

    /// Add a line at order `index` (0 is below every other line), splitting
    /// band `index` in two: both halves keep its material and texture rule.
    /// Returns false when the set is full or `index` is out of range.
    pub fn insert_line(&mut self, index: usize, anchor: Anchor) -> bool {
        if self.lines.len() >= MAX_BAND_LINES || index > self.lines.len() || !self.is_valid() {
            return false;
        }
        self.lines.insert(index, anchor);
        let band = self.bands[index].clone();
        self.bands.insert(index, band);
        true
    }

    /// Remove line `index`, merging the band below it with the band above
    /// (the lower band's material and rule win). Returns false when there is
    /// no such line.
    pub fn remove_line(&mut self, index: usize) -> bool {
        if index >= self.lines.len() || !self.is_valid() {
            return false;
        }
        self.lines.remove(index);
        self.bands.remove(index + 1);
        true
    }

    /// Join band `other` to the band `keep` next to it, removing the cut
    /// between them. The joined band is `keep`'s: its material and texture
    /// rule stay. Returns false when the two are not neighbours.
    pub fn merge_bands(&mut self, keep: usize, other: usize) -> bool {
        if !self.is_valid() || keep.max(other) >= self.bands.len() || keep.abs_diff(other) != 1 {
            return false;
        }
        let kept = self.bands[keep].clone();
        let line = keep.min(other);
        if !self.remove_line(line) {
            return false;
        }
        self.bands[line] = kept;
        true
    }

    /// Replace the anchor of line `index`.
    pub fn set_line(&mut self, index: usize, anchor: Anchor) -> bool {
        match self.lines.get_mut(index) {
            Some(slot) if *slot != anchor => {
                *slot = anchor;
                true
            }
            _ => false,
        }
    }

    /// Set the material of band `index`.
    pub fn set_band_material(&mut self, index: usize, material: Option<ResourceId>) -> bool {
        match self.bands.get_mut(index) {
            Some(band) if band.material != material => {
                band.material = material;
                true
            }
            _ => false,
        }
    }
}

/// The set a wall uses, given its own assignment (`None` falls to the room
/// default, [`NO_BANDS`] or an index out of range means none).
pub fn resolve(room: &GridRoom, assigned: Option<u8>) -> Option<(u8, &BandSet)> {
    let index = assigned.or(room.defaults.bands)?;
    if index == NO_BANDS {
        return None;
    }
    let set = room.band_sets.get(usize::from(index))?;
    (set.is_valid() && !set.is_plain()).then_some((index, set))
}

/// Whether any wall of `room` can be banded, so the builders know whether
/// the neighbour stitching has anything to do.
pub fn room_has_bands(room: &GridRoom) -> bool {
    room.band_sets
        .iter()
        .any(|s| !s.lines.is_empty() || !s.is_plain())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(n: u64) -> ResourceId {
        ron::from_str(&format!("({n})")).expect("a resource id")
    }

    #[test]
    fn inserting_and_removing_lines_keeps_bands_one_more_than_lines() {
        let mut set = BandSet::single("walls", Some(rid(1)));
        assert!(set.is_valid() && !set.is_plain());
        assert!(set.insert_line(0, Anchor::Bottom(256)));
        assert!(set.insert_line(1, Anchor::Top(256)));
        assert_eq!(set.bands.len(), 3);
        assert!(set.is_valid());
        // Both halves of a split band keep its material.
        assert!(set.bands.iter().all(|b| b.material == Some(rid(1))));
        assert!(set.set_band_material(0, Some(rid(2))));
        assert!(set.set_band_material(2, Some(rid(3))));
        assert!(set.remove_line(0));
        // The lower band wins the merge: band 0 was rid(2).
        assert_eq!(set.bands[0].material, Some(rid(2)));
        assert_eq!(set.bands[1].material, Some(rid(3)));
        assert!(!set.remove_line(5));
    }

    #[test]
    fn a_set_is_capped_at_six_lines() {
        let mut set = BandSet::single("s", None);
        for i in 0..MAX_BAND_LINES {
            assert!(set.insert_line(i, Anchor::Abs(256 * (i as i32 + 1))));
        }
        assert!(!set.insert_line(0, Anchor::Abs(1)));
        assert_eq!(set.bands.len(), MAX_BAND_LINES + 1);
    }

    #[test]
    fn a_lone_unstyled_band_is_plain_and_resolves_to_nothing() {
        let mut room = GridRoom::new("r", 1, 1);
        room.band_sets.push(BandSet::single("plain", None));
        room.defaults.bands = Some(0);
        assert!(resolve(&room, None).is_none());
        room.band_sets.push(BandSet::single("tinted", Some(rid(4))));
        assert_eq!(resolve(&room, Some(1)).map(|(i, _)| i), Some(1));
        // Explicit none beats the room default.
        room.band_sets[0].insert_line(0, Anchor::Bottom(256));
        assert_eq!(resolve(&room, None).map(|(i, _)| i), Some(0));
        assert!(resolve(&room, Some(NO_BANDS)).is_none());
        // An index past the list is ignored, not a panic.
        assert!(resolve(&room, Some(9)).is_none());
    }

    #[test]
    fn sets_round_trip_through_ron_and_defaults_stay_out_of_the_file() {
        let mut set = BandSet::single("s", Some(rid(3)));
        set.insert_line(0, Anchor::Top(512));
        set.bands[1].v = BandV::FromTop;
        let text = ron::to_string(&set).unwrap();
        let back: BandSet = ron::from_str(&text).unwrap();
        assert_eq!(back, set);
        assert!(!text.contains("uv"), "{text}");
        assert!(text.contains("FromTop"), "{text}");
        assert!(!ron::to_string(&Band::default()).unwrap().contains("uv"));
    }
}
