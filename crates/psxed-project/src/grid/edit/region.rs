//! The region the level verbs act on: a set of world sectors of one room.
//!
//! Interaction is region, then verb, then amount. A region comes from a
//! marquee (a rectangle of sectors), a paint brush (a disc of sectors) or a
//! path (waypoints and a width), and it persists after a verb so verbs chain.
//! Regions are editor state, never saved. The functions here are pure so the
//! tests and the UI build the same sets.

use std::collections::BTreeSet;

use super::super::world::GridWorld;
use super::rects::CellRect;

/// Whether the step from `a` to the adjacent cell `b` runs along X.
fn step(a: [i32; 2], b: [i32; 2]) -> bool {
    a[1] == b[1] && a[0] != b[0]
}

/// A set of cells of one level in one room: world sectors (the default), or
/// halves or quarters of a sector, each addressed by its cell coordinate at
/// that level (`world click / edge`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    /// The room the cells belong to.
    pub room: usize,
    cells: BTreeSet<[i32; 2]>,
    /// The cells' level: 2 sectors, 3 halves, 4 quarters.
    level: u8,
}

impl Default for Region {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Region {
    /// An empty region in `room`.
    pub fn new(room: usize) -> Self {
        Self {
            room,
            cells: BTreeSet::new(),
            level: super::super::leaf::SECTOR_LEVEL,
        }
    }

    /// An empty region of cells of `level` (2 sectors, 3 halves, 4
    /// quarters) in `room`.
    pub fn new_at(room: usize, level: u8) -> Self {
        Self {
            level: level.clamp(super::super::leaf::SECTOR_LEVEL, super::super::leaf::FINEST_LEVEL),
            ..Self::new(room)
        }
    }

    /// The level of the cells: 2 sectors, 3 halves, 4 quarters.
    pub fn level(&self) -> u8 {
        self.level
    }

    /// The same cells read as cells of `level`.
    pub fn at_level(mut self, level: u8) -> Self {
        self.level = level.clamp(super::super::leaf::SECTOR_LEVEL, super::super::leaf::FINEST_LEVEL);
        self
    }

    /// Whether the verbs must act on cells through the resolution lens: the
    /// region's cells are smaller than a sector, or the room has cells of
    /// several sizes.
    pub fn is_fine(&self, world: &GridWorld) -> bool {
        self.level != super::super::leaf::SECTOR_LEVEL
            || world
                .rooms
                .get(self.room)
                .is_some_and(|r| !r.leaves.is_sectors_only())
    }

    /// A region of the given cells.
    pub fn from_cells(room: usize, cells: impl IntoIterator<Item = [i32; 2]>) -> Self {
        Self {
            room,
            cells: cells.into_iter().collect(),
            level: super::super::leaf::SECTOR_LEVEL,
        }
    }

    /// Every sector of `rect`.
    pub fn from_rect(room: usize, rect: CellRect) -> Self {
        Self::from_cells(room, rect.cells())
    }

    /// A paint brush stroke: the sectors within `radius` sectors of `centre`
    /// (radius 0 is the sector itself, 1 a plus of five, 2 a disc of 13).
    pub fn brush(room: usize, centre: [i32; 2], radius: i32) -> Self {
        let radius = radius.max(0);
        let mut cells = BTreeSet::new();
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dz * dz <= radius * radius {
                    cells.insert([centre[0] + dx, centre[1] + dz]);
                }
            }
        }
        Self {
            room,
            cells,
            level: super::super::leaf::SECTOR_LEVEL,
        }
    }

    /// The sectors a path through `waypoints` crosses, in order and four
    /// connected (each step is one sector east, west, north or south).
    /// Consecutive repeats are dropped.
    pub fn path_cells(waypoints: &[[i32; 2]]) -> Vec<[i32; 2]> {
        let mut out: Vec<[i32; 2]> = Vec::new();
        let mut push = |cell: [i32; 2]| {
            if out.last() != Some(&cell) {
                out.push(cell);
            }
        };
        let Some(first) = waypoints.first() else {
            return out;
        };
        push(*first);
        for pair in waypoints.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (nx, nz) = ((b[0] - a[0]).abs(), (b[1] - a[1]).abs());
            let (sx, sz) = ((b[0] - a[0]).signum(), (b[1] - a[1]).signum());
            let (mut at, mut ix, mut iz) = (a, 0, 0);
            while ix < nx || iz < nz {
                // The classic four-connected line walk: step along X while
                // the line is closer to the next column than the next row.
                if (1 + 2 * ix) * nz < (1 + 2 * iz) * nx {
                    ix += 1;
                    at[0] += sx;
                } else {
                    iz += 1;
                    at[1] += sz;
                }
                push(at);
            }
        }
        out
    }

    /// The sectors within a path of `width` sectors through `waypoints`: the
    /// path widened sideways (across each leg, not past its ends).
    pub fn from_path(room: usize, waypoints: &[[i32; 2]], width: i32) -> Self {
        let width = width.max(1);
        let (before, after) = ((width - 1) / 2, width / 2);
        let path = Self::path_cells(waypoints);
        let mut cells = BTreeSet::new();
        for (i, cell) in path.iter().enumerate() {
            // The legs this sector sits on: the one it arrives by and the
            // one it leaves by. A lone sector widens both ways.
            let incoming = i.checked_sub(1).map(|p| step(path[p], *cell));
            let outgoing = path.get(i + 1).map(|n| step(*cell, *n));
            let mut axes: Vec<bool> = [incoming, outgoing].into_iter().flatten().collect();
            if axes.is_empty() {
                axes = vec![true, false];
            }
            for along_x in axes {
                for k in -before..=after {
                    // A leg along X widens in Z and the other way round.
                    cells.insert(if along_x {
                        [cell[0], cell[1] + k]
                    } else {
                        [cell[0] + k, cell[1]]
                    });
                }
            }
        }
        Self {
            room,
            cells,
            level: super::super::leaf::SECTOR_LEVEL,
        }
    }

    /// Add every sector of `other` (same room expected).
    pub fn extend(&mut self, other: &Region) {
        self.cells.extend(other.cells.iter().copied());
    }

    /// The sectors, ordered by `x` then `z`.
    pub fn cells(&self) -> impl Iterator<Item = [i32; 2]> + '_ {
        self.cells.iter().copied()
    }

    /// Number of sectors.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether the region holds nothing.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Whether `cell` is in the region.
    pub fn contains(&self, cell: [i32; 2]) -> bool {
        self.cells.contains(&cell)
    }

    /// The smallest rectangle holding every sector.
    pub fn bounds(&self) -> Option<CellRect> {
        let mut it = self.cells.iter();
        let first = *it.next()?;
        let mut rect = CellRect {
            min: first,
            max: first,
        };
        for c in it {
            rect.min = [rect.min[0].min(c[0]), rect.min[1].min(c[1])];
            rect.max = [rect.max[0].max(c[0]), rect.max[1].max(c[1])];
        }
        Some(rect)
    }

    /// The sectors that have a floor in the room, and how many do not.
    pub fn with_floors(&self, world: &GridWorld) -> (Vec<[i32; 2]>, usize) {
        let Some(room) = world.rooms.get(self.room) else {
            return (Vec::new(), self.cells.len());
        };
        let mut kept = Vec::new();
        let mut skipped = 0;
        for cell in self.cells() {
            let has = room.local(cell).is_some()
                && room
                    .leaves
                    .in_sector(cell)
                    .iter()
                    .any(|(_, s)| s.ground_floor().is_some());
            if has {
                kept.push(cell);
            } else {
                skipped += 1;
            }
        }
        (kept, skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marquee_is_its_rectangle() {
        let r = Region::from_rect(0, CellRect::from_corners([2, 1], [4, 2]));
        assert_eq!(r.len(), 6);
        assert!(r.contains([4, 2]) && !r.contains([5, 2]));
        let b = r.bounds().unwrap();
        assert_eq!((b.min, b.max), ([2, 1], [4, 2]));
    }

    #[test]
    fn a_brush_is_a_disc_of_sectors() {
        assert_eq!(Region::brush(0, [5, 5], 0).len(), 1);
        assert_eq!(Region::brush(0, [5, 5], 1).len(), 5);
        assert_eq!(Region::brush(0, [5, 5], 2).len(), 13);
        assert!(Region::brush(0, [5, 5], 1).contains([5, 4]));
        assert!(!Region::brush(0, [5, 5], 1).contains([6, 6]));
    }

    #[test]
    fn a_path_is_four_connected_ordered_and_deterministic() {
        let cells = Region::path_cells(&[[0, 0], [3, 2]]);
        assert_eq!(cells.first(), Some(&[0, 0]));
        assert_eq!(cells.last(), Some(&[3, 2]));
        assert_eq!(cells.len(), 6, "dx + dz + 1 cells: {cells:?}");
        for pair in cells.windows(2) {
            let d = (pair[1][0] - pair[0][0]).abs() + (pair[1][1] - pair[0][1]).abs();
            assert_eq!(d, 1, "{cells:?}");
        }
        assert_eq!(cells, Region::path_cells(&[[0, 0], [3, 2]]));
        // A straight leg and an L through a waypoint.
        assert_eq!(
            Region::path_cells(&[[1, 1], [1, 4]]),
            vec![[1, 1], [1, 2], [1, 3], [1, 4]]
        );
        assert_eq!(
            Region::path_cells(&[[0, 0], [2, 0], [2, 2]]),
            vec![[0, 0], [1, 0], [2, 0], [2, 1], [2, 2]]
        );
        // A repeated waypoint adds nothing.
        assert_eq!(Region::path_cells(&[[0, 0], [0, 0]]), vec![[0, 0]]);
    }

    #[test]
    fn a_path_region_widens_around_the_line() {
        assert_eq!(Region::from_path(0, &[[0, 0], [3, 0]], 1).len(), 4);
        assert_eq!(Region::from_path(0, &[[0, 0], [3, 0]], 3).len(), 12);
        // An even width leans one sector to the far side, deterministically.
        let two = Region::from_path(0, &[[0, 0], [3, 0]], 2);
        assert_eq!(two.len(), 8);
        assert!(two.contains([0, 1]) && !two.contains([0, -1]));
    }
}
