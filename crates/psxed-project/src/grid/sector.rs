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

/// A sector. Both surfaces and every wall list are optional, so an empty
/// `Some(GridSector::default())` is a valid placeholder cell.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSector {
    /// Floor, `None` for a pit or open void.
    #[serde(default)]
    pub floor: Option<GridSurface>,
    /// Ceiling, `None` for open sky.
    #[serde(default)]
    pub ceiling: Option<GridSurface>,
    /// Walls on the north edge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub north: Vec<GridWall>,
    /// Walls on the east edge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub east: Vec<GridWall>,
    /// Walls on the south edge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub south: Vec<GridWall>,
    /// Walls on the west edge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub west: Vec<GridWall>,
    /// Walls on the NW-SE diagonal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nwse: Vec<GridWall>,
    /// Walls on the NE-SW diagonal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nesw: Vec<GridWall>,
    /// Material of the derived walls this sector owns (a platform's sides,
    /// a closure wall). `None` falls through to the room default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_material: Option<ResourceId>,
    /// How rough the rock around this sector looks, 0 (none) to 3: the
    /// render-only jitter and tint of [`super::rough`]. Two bits.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub rough: u8,
}

fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

impl GridSector {
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
        self.floor.is_none() && self.ceiling.is_none() && self.wall_count() == 0
    }

    /// Lowest and highest geometry height in the sector, or `None` when it
    /// holds no surface.
    pub fn height_range(&self) -> Option<(i32, i32)> {
        let mut range: Option<(i32, i32)> = None;
        let mut take = |(lo, hi): (i32, i32)| {
            range = Some(match range {
                Some((a, b)) => (a.min(lo), b.max(hi)),
                None => (lo, hi),
            });
        };
        for surface in [&self.floor, &self.ceiling].into_iter().flatten() {
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
