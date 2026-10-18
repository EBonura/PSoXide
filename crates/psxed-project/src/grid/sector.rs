//! One sector: a floor, a ceiling and the wall stacks on its six edges.

use serde::{Deserialize, Serialize};

use crate::ResourceId;

use super::surface::{GridSurface, GridWall};

/// One of the six places a wall can stand in a sector.
///
/// The four cardinal edges are the sector's sides; the two diagonals cut it
/// corner to corner. A wall record lives in exactly one sector's list for one
/// edge, so ownership is never ambiguous: the wall between sector A and its
/// eastern neighbour B is either in A's `East` list or in B's `West` list,
/// each with its own visible side. Use [`WallEdge::opposite`] to find the
/// other candidate.
///
/// Compass: north is `-Z`, east is `+X`, matching the bonnie-32 grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WallEdge {
    /// Edge on the `-Z` side.
    North,
    /// Edge on the `+X` side.
    East,
    /// Edge on the `+Z` side.
    South,
    /// Edge on the `-X` side.
    West,
    /// Diagonal from the north-west corner to the south-east corner.
    Nwse,
    /// Diagonal from the north-east corner to the south-west corner.
    Nesw,
}

impl WallEdge {
    /// Every edge in storage order.
    pub const ALL: [Self; 6] = [
        Self::North,
        Self::East,
        Self::South,
        Self::West,
        Self::Nwse,
        Self::Nesw,
    ];

    /// Walking direction endpoints `a -> b` as sector-local corner offsets
    /// in whole sectors, `[x, z]` with each coordinate `0` or `1`.
    ///
    /// The cardinal edges run clockwise around the sector as seen from above
    /// with north at the top, so the interior is always on the right-hand
    /// side and [`Self::front_direction`] points into it.
    pub const fn endpoints(self) -> ([i32; 2], [i32; 2]) {
        match self {
            Self::North => ([0, 0], [1, 0]),
            Self::East => ([1, 0], [1, 1]),
            Self::South => ([1, 1], [0, 1]),
            Self::West => ([0, 1], [0, 0]),
            Self::Nwse => ([0, 0], [1, 1]),
            Self::Nesw => ([1, 0], [0, 1]),
        }
    }

    /// Horizontal direction `[x, z]` the front side faces, not normalised
    /// (the diagonals have length root two).
    pub const fn front_direction(self) -> [i32; 2] {
        match self {
            Self::North => [0, 1],
            Self::East => [-1, 0],
            Self::South => [0, -1],
            Self::West => [1, 0],
            Self::Nwse => [1, -1],
            Self::Nesw => [-1, -1],
        }
    }

    /// The edge of the neighbouring sector that is the same physical line,
    /// with the neighbour's sector offset. `None` for the diagonals, which
    /// have no neighbour.
    pub const fn opposite(self) -> Option<(Self, [i32; 2])> {
        match self {
            Self::North => Some((Self::South, [0, -1])),
            Self::East => Some((Self::West, [1, 0])),
            Self::South => Some((Self::North, [0, 1])),
            Self::West => Some((Self::East, [-1, 0])),
            Self::Nwse | Self::Nesw => None,
        }
    }

    /// Whether this is one of the two diagonals.
    pub const fn is_diagonal(self) -> bool {
        matches!(self, Self::Nwse | Self::Nesw)
    }

    /// Lower-case label for the UI and diagnostics.
    pub const fn label(self) -> &'static str {
        match self {
            Self::North => "north",
            Self::East => "east",
            Self::South => "south",
            Self::West => "west",
            Self::Nwse => "diagonal NW-SE",
            Self::Nesw => "diagonal NE-SW",
        }
    }
}

/// Most spans in one column. Three bits in the leaf format (`docs/grid/
/// multi-storey.md` 4.2 item 4; a proposal until a level needs more).
pub const MAX_SPANS_PER_CELL: usize = 8;

/// One open volume of a column: a floor surface, a ceiling surface and the
/// material of the solid directly below the floor.
///
/// Every level made so far is a column of exactly one span. The floor of the
/// lowest span may be absent (a pit) and the ceiling of the highest too
/// (open sky); between two spans lies solid.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSpan {
    /// Floor, `None` for a pit or open void.
    #[serde(default)]
    pub floor: Option<GridSurface>,
    /// Ceiling, `None` for open sky.
    #[serde(default)]
    pub ceiling: Option<GridSurface>,
    /// Material of the derived walls the solid below this span's floor
    /// owns (a platform's sides, a closure wall). `None` falls through to
    /// the room default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_material: Option<ResourceId>,
}

impl GridSpan {
    /// Whether the span has neither surface: a placeholder.
    pub fn is_vacant(&self) -> bool {
        self.floor.is_none() && self.ceiling.is_none()
    }
}

/// A leaf of the room: a column of spans and the authored walls of the
/// cell.
///
/// A leaf of level 2 is exactly today's sector. The column is never empty:
/// a cell with no surface at all holds one vacant span, so
/// `GridSector::default()` is a valid placeholder and "the ground span"
/// always exists.
///
/// The spans are private so that every consumer says what it means
/// (`docs/grid/multi-storey.md` 4.3). [`Self::floor`], [`Self::ceiling`]
/// and [`Self::wall_material`] and their setters act on the **ground span**,
/// span 0, which is the whole column in every level made so far; the tools,
/// derivation, faces, light, cook and walkability go through them and so see
/// the ground span only. [`limits::validate`](super::limits::validate)
/// refuses a world whose columns hold more until those consumers read all
/// the spans. Aggregate readers (bounds, materials, validation) use
/// [`Self::surfaces`] and [`Self::spans`].
///
/// On disk a column of one span is written with the three plain fields
/// (`floor`, `ceiling`, `wall_material`), exactly as before spans existed;
/// the spans above the ground one go in `above`, which is left out while
/// empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridSector {
    /// Bottom to top. Never empty.
    spans: Vec<GridSpan>,
    /// Walls on the north edge.
    pub north: Vec<GridWall>,
    /// Walls on the east edge.
    pub east: Vec<GridWall>,
    /// Walls on the south edge.
    pub south: Vec<GridWall>,
    /// Walls on the west edge.
    pub west: Vec<GridWall>,
    /// Walls on the NW-SE diagonal.
    pub nwse: Vec<GridWall>,
    /// Walls on the NE-SW diagonal.
    pub nesw: Vec<GridWall>,
    /// How rough the rock around this sector looks, 0 (none) to 3: the
    /// render-only jitter and tint of [`super::rough`]. Two bits.
    pub rough: u8,
}

impl Default for GridSector {
    fn default() -> Self {
        Self {
            spans: vec![GridSpan::default()],
            north: Vec::new(),
            east: Vec::new(),
            south: Vec::new(),
            west: Vec::new(),
            nwse: Vec::new(),
            nesw: Vec::new(),
            rough: 0,
        }
    }
}

impl GridSector {
    /// A sector whose ground span has `floor` and `ceiling`.
    pub fn with_surfaces(floor: Option<GridSurface>, ceiling: Option<GridSurface>) -> Self {
        let mut sector = Self::default();
        sector.spans[0].floor = floor;
        sector.spans[0].ceiling = ceiling;
        sector
    }

    // ---- the column --------------------------------------------------

    /// The spans of the column, bottom to top. Never empty.
    pub fn spans(&self) -> &[GridSpan] {
        &self.spans
    }

    /// Mutable spans, bottom to top. The count cannot change through this;
    /// use [`Self::insert_span`] and [`Self::remove_span`].
    pub fn spans_mut(&mut self) -> &mut [GridSpan] {
        &mut self.spans
    }

    /// Number of spans, at least 1.
    pub fn span_count(&self) -> usize {
        self.spans.len()
    }

    /// Whether the column is the single span every level made so far has.
    pub fn is_single_span(&self) -> bool {
        self.spans.len() == 1
    }

    /// Insert `span` at `index` (clamped to the end), pushing the spans
    /// from `index` up. The caller keeps the column ordered; there is no
    /// check here.
    pub fn insert_span(&mut self, index: usize, span: GridSpan) {
        let index = index.min(self.spans.len());
        self.spans.insert(index, span);
    }

    /// Remove and return span `index`. The last span of a column cannot be
    /// removed: it is replaced by a vacant one, so the column is never
    /// empty.
    pub fn remove_span(&mut self, index: usize) -> Option<GridSpan> {
        if index >= self.spans.len() {
            return None;
        }
        if self.spans.len() == 1 {
            return Some(core::mem::take(&mut self.spans[0]));
        }
        Some(self.spans.remove(index))
    }

    /// Every floor and ceiling of the column, bottom to top (a span's floor
    /// before its ceiling). Aggregate readers use this; a reader that builds
    /// geometry from the ground span uses [`Self::floor`] and
    /// [`Self::ceiling`].
    pub fn surfaces(&self) -> impl Iterator<Item = &GridSurface> + '_ {
        self.spans
            .iter()
            .flat_map(|span| span.floor.iter().chain(span.ceiling.iter()))
    }

    /// Mutable form of [`Self::surfaces`].
    pub fn surfaces_mut(&mut self) -> impl Iterator<Item = &mut GridSurface> + '_ {
        self.spans
            .iter_mut()
            .flat_map(|span| span.floor.iter_mut().chain(span.ceiling.iter_mut()))
    }

    /// The span a working height selects, the one resolver every consumer
    /// of a multi-span column asks (`docs/grid/multi-storey.md` 4.3).
    ///
    /// A span is a candidate when its floor reaches no higher than
    /// `y + reach` at its lowest corner (a pit has no floor and always
    /// does) and its ceiling is above `y` at its highest corner (open sky
    /// always is). `reach` is the step-up, so the first treads of a flight
    /// belong to the storey they leave. Of the candidates the one with the
    /// highest floor wins; a span with no floor ranks below every floor.
    /// Heights are in grid units relative to the room offset, as stored.
    pub fn span_at(&self, y: i32, reach: i32) -> Option<usize> {
        let reach_y = y.saturating_add(reach);
        let mut best: Option<(usize, i32)> = None;
        for (index, span) in self.spans.iter().enumerate() {
            let floor = span.floor.as_ref().map(|f| f.height_range().0);
            if floor.is_some_and(|f| f > reach_y) {
                continue;
            }
            if span
                .ceiling
                .as_ref()
                .is_some_and(|c| c.height_range().1 <= y)
            {
                continue;
            }
            let rank = floor.unwrap_or(i32::MIN);
            if best.is_none_or(|(_, r)| rank >= r) {
                best = Some((index, rank));
            }
        }
        best.map(|(index, _)| index)
    }

    // ---- the ground span ---------------------------------------------

    /// Floor of the ground span, `None` for a pit or open void.
    pub fn floor(&self) -> Option<&GridSurface> {
        self.spans[0].floor.as_ref()
    }

    /// Ceiling of the ground span, `None` for open sky.
    pub fn ceiling(&self) -> Option<&GridSurface> {
        self.spans[0].ceiling.as_ref()
    }

    /// Mutable floor of the ground span.
    pub fn floor_mut(&mut self) -> Option<&mut GridSurface> {
        self.spans[0].floor.as_mut()
    }

    /// Mutable ceiling of the ground span.
    pub fn ceiling_mut(&mut self) -> Option<&mut GridSurface> {
        self.spans[0].ceiling.as_mut()
    }

    /// The floor slot of the ground span itself, for code that treats floor
    /// and ceiling alike (see [`Self::ceiling_slot_mut`]).
    pub fn floor_slot_mut(&mut self) -> &mut Option<GridSurface> {
        &mut self.spans[0].floor
    }

    /// The ceiling slot of the ground span itself.
    pub fn ceiling_slot_mut(&mut self) -> &mut Option<GridSurface> {
        &mut self.spans[0].ceiling
    }

    /// The ground span's floor and ceiling at once, both mutable (two calls
    /// to [`Self::floor_mut`] and [`Self::ceiling_mut`] would borrow the
    /// sector twice).
    pub fn floor_and_ceiling_mut(
        &mut self,
    ) -> (Option<&mut GridSurface>, Option<&mut GridSurface>) {
        let ground = &mut self.spans[0];
        (ground.floor.as_mut(), ground.ceiling.as_mut())
    }

    /// Replace the floor of the ground span.
    pub fn set_floor(&mut self, floor: Option<GridSurface>) {
        self.spans[0].floor = floor;
    }

    /// Replace the ceiling of the ground span.
    pub fn set_ceiling(&mut self, ceiling: Option<GridSurface>) {
        self.spans[0].ceiling = ceiling;
    }

    /// Take the floor of the ground span, leaving none.
    pub fn take_floor(&mut self) -> Option<GridSurface> {
        self.spans[0].floor.take()
    }

    /// Take the ceiling of the ground span, leaving none.
    pub fn take_ceiling(&mut self) -> Option<GridSurface> {
        self.spans[0].ceiling.take()
    }

    /// Material of the derived walls the ground span's solid owns.
    pub fn wall_material(&self) -> Option<ResourceId> {
        self.spans[0].wall_material
    }

    /// Set the material of the derived walls the ground span's solid owns.
    pub fn set_wall_material(&mut self, material: Option<ResourceId>) {
        self.spans[0].wall_material = material;
    }

    // ---- walls -------------------------------------------------------

    /// Wall stack on `edge`.
    pub fn walls(&self, edge: WallEdge) -> &Vec<GridWall> {
        match edge {
            WallEdge::North => &self.north,
            WallEdge::East => &self.east,
            WallEdge::South => &self.south,
            WallEdge::West => &self.west,
            WallEdge::Nwse => &self.nwse,
            WallEdge::Nesw => &self.nesw,
        }
    }

    /// Mutable wall stack on `edge`.
    pub fn walls_mut(&mut self, edge: WallEdge) -> &mut Vec<GridWall> {
        match edge {
            WallEdge::North => &mut self.north,
            WallEdge::East => &mut self.east,
            WallEdge::South => &mut self.south,
            WallEdge::West => &mut self.west,
            WallEdge::Nwse => &mut self.nwse,
            WallEdge::Nesw => &mut self.nesw,
        }
    }

    /// Total wall segments over all six edges.
    pub fn wall_count(&self) -> usize {
        WallEdge::ALL.iter().map(|e| self.walls(*e).len()).sum()
    }

    /// Whether the sector holds nothing at all.
    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(GridSpan::is_vacant) && self.wall_count() == 0
    }

    /// Lowest and highest geometry height in the sector, over every span,
    /// or `None` when it holds no surface.
    pub fn height_range(&self) -> Option<(i32, i32)> {
        let mut range: Option<(i32, i32)> = None;
        let mut take = |(lo, hi): (i32, i32)| {
            range = Some(match range {
                Some((a, b)) => (a.min(lo), b.max(hi)),
                None => (lo, hi),
            });
        };
        for surface in self.surfaces() {
            take(surface.height_range());
        }
        for edge in WallEdge::ALL {
            for wall in self.walls(edge) {
                take(wall.height_range());
            }
        }
        range
    }
}

/// The disk form of a sector: the plain fields of a one-span column, plus
/// `above` for the spans over the ground one.
#[derive(Deserialize)]
struct SectorDoc {
    #[serde(default)]
    floor: Option<GridSurface>,
    #[serde(default)]
    ceiling: Option<GridSurface>,
    #[serde(default)]
    north: Vec<GridWall>,
    #[serde(default)]
    east: Vec<GridWall>,
    #[serde(default)]
    south: Vec<GridWall>,
    #[serde(default)]
    west: Vec<GridWall>,
    #[serde(default)]
    nwse: Vec<GridWall>,
    #[serde(default)]
    nesw: Vec<GridWall>,
    #[serde(default)]
    wall_material: Option<ResourceId>,
    #[serde(default)]
    rough: u8,
    #[serde(default)]
    above: Vec<GridSpan>,
}

impl Serialize for GridSector {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let ground = &self.spans[0];
        let walls = WallEdge::ALL.map(|e| self.walls(e));
        let fields = 2
            + walls.iter().filter(|w| !w.is_empty()).count()
            + usize::from(ground.wall_material.is_some())
            + usize::from(self.rough != 0)
            + usize::from(self.spans.len() > 1);
        let mut out = serializer.serialize_struct("GridSector", fields)?;
        out.serialize_field("floor", &ground.floor)?;
        out.serialize_field("ceiling", &ground.ceiling)?;
        for (name, list) in ["north", "east", "south", "west", "nwse", "nesw"]
            .into_iter()
            .zip(walls)
        {
            if !list.is_empty() {
                out.serialize_field(name, list)?;
            }
        }
        if let Some(material) = &ground.wall_material {
            out.serialize_field("wall_material", material)?;
        }
        if self.rough != 0 {
            out.serialize_field("rough", &self.rough)?;
        }
        if self.spans.len() > 1 {
            out.serialize_field("above", &self.spans[1..])?;
        }
        out.end()
    }
}

impl<'de> Deserialize<'de> for GridSector {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let doc = SectorDoc::deserialize(deserializer)?;
        let mut spans = Vec::with_capacity(1 + doc.above.len());
        spans.push(GridSpan {
            floor: doc.floor,
            ceiling: doc.ceiling,
            wall_material: doc.wall_material,
        });
        spans.extend(doc.above);
        Ok(Self {
            spans,
            north: doc.north,
            east: doc.east,
            south: doc.south,
            west: doc.west,
            nwse: doc.nwse,
            nesw: doc.nesw,
            rough: doc.rough,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposite_is_an_involution() {
        for edge in WallEdge::ALL {
            if let Some((other, offset)) = edge.opposite() {
                let (back, back_offset) = other.opposite().expect("cardinal pair");
                assert_eq!(back, edge);
                assert_eq!(back_offset, [-offset[0], -offset[1]]);
            } else {
                assert!(edge.is_diagonal());
            }
        }
    }

    #[test]
    fn opposite_edges_are_the_same_physical_segment() {
        // A's endpoints, walked backwards, are B's endpoints shifted by the
        // neighbour offset.
        for edge in [
            WallEdge::North,
            WallEdge::East,
            WallEdge::South,
            WallEdge::West,
        ] {
            let (other, offset) = edge.opposite().unwrap();
            let (a0, a1) = edge.endpoints();
            let (b0, b1) = other.endpoints();
            let shift = |p: [i32; 2]| [p[0] + offset[0], p[1] + offset[1]];
            assert_eq!([shift(b0), shift(b1)], [a1, a0], "{edge:?}");
        }
    }

    #[test]
    fn front_faces_the_interior() {
        // The interior is on the right of the walking direction a -> b with
        // z pointing down the page, i.e. the front direction rotated a
        // quarter turn clockwise from the walking direction.
        for edge in [
            WallEdge::North,
            WallEdge::East,
            WallEdge::South,
            WallEdge::West,
        ] {
            let (a, b) = edge.endpoints();
            let walk = [b[0] - a[0], b[1] - a[1]];
            let right = [-walk[1], walk[0]];
            assert_eq!(edge.front_direction(), right, "{edge:?}");
        }
    }

    #[test]
    fn wall_lists_are_addressed_by_edge() {
        let mut sector = GridSector::default();
        for (i, edge) in WallEdge::ALL.into_iter().enumerate() {
            sector
                .walls_mut(edge)
                .push(GridWall::span(0, 256 * (i as i32 + 1), None));
        }
        assert_eq!(sector.wall_count(), 6);
        assert_eq!(sector.walls(WallEdge::Nesw)[0].heights[2], 256 * 6);
        assert_eq!(sector.north[0].heights[2], 256);
        assert_eq!(sector.height_range(), Some((0, 1536)));
        assert!(!sector.is_empty());
        assert!(GridSector::default().is_empty());
    }
}
