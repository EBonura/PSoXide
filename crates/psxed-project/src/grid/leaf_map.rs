//! The leaf map: a room's cells as a linear quadtree.
//!
//! Leaves are disjoint and aligned, interior nodes are implicit (a node
//! exists while some leaf lies below it) and an uncovered area is void.
//! Iteration is Morton order of the leaf corner with the level as the
//! tiebreak, so serialisation and every tool that walks the map are
//! deterministic (`docs/grid/level-tools-design.md` 3.2).
//!
//! The map is keyed by [`LeafKey`] in world clicks, so the room rectangle
//! plays no part in it: growing, trimming or moving the origin never touches
//! a key. A leaf value is a [`GridSector`], the column of spans and the
//! authored walls of the cell (`docs/grid/multi-storey.md` 4.1).

use std::collections::BTreeMap;

use super::leaf::{LeafKey, LeafOrder, SECTOR_LEVEL};
use super::sector::GridSector;

/// One stored leaf.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Leaf {
    key: LeafKey,
    sector: GridSector,
}

/// Why a leaf could not be stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafError {
    /// The level is out of range or the corner is not aligned to the
    /// leaf's own lattice.
    Misaligned(LeafKey),
    /// The leaf overlaps this existing, different leaf (one holds the
    /// other, because aligned leaves never cross).
    Overlaps(LeafKey),
}

impl core::fmt::Display for LeafError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Misaligned(k) => write!(
                f,
                "leaf level {} at clicks ({}, {}) is not aligned to its size",
                k.level, k.x, k.z
            ),
            Self::Overlaps(k) => write!(
                f,
                "leaf overlaps the leaf of level {} at clicks ({}, {})",
                k.level, k.x, k.z
            ),
        }
    }
}

impl std::error::Error for LeafError {}

/// The leaves of one room.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LeafMap {
    leaves: BTreeMap<LeafOrder, Leaf>,
}

impl LeafMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of leaves of every level.
    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    /// Whether the map holds no leaf.
    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    /// Drop every leaf.
    pub fn clear(&mut self) {
        self.leaves.clear();
    }

    /// The leaf stored under exactly `key`.
    pub fn get(&self, key: LeafKey) -> Option<&GridSector> {
        self.leaves.get(&key.order()).map(|leaf| &leaf.sector)
    }

    /// Mutable leaf stored under exactly `key`.
    pub fn get_mut(&mut self, key: LeafKey) -> Option<&mut GridSector> {
        self.leaves
            .get_mut(&key.order())
            .map(|leaf| &mut leaf.sector)
    }

    /// Whether a leaf is stored under exactly `key`.
    pub fn contains(&self, key: LeafKey) -> bool {
        self.leaves.contains_key(&key.order())
    }

    /// The leaf that covers the click `click`, of any level, with its key.
    /// Tries the sector level first: it is what every level made before
    /// the quadtree holds.
    pub fn leaf_at(&self, click: [i32; 2]) -> Option<(LeafKey, &GridSector)> {
        for level in [SECTOR_LEVEL, 3, 4, 1, 0] {
            let key = LeafKey::containing(level, click);
            if let Some(sector) = self.get(key) {
                return Some((key, sector));
            }
        }
        None
    }

    /// Mutable form of [`Self::leaf_at`].
    pub fn leaf_at_mut(&mut self, click: [i32; 2]) -> Option<(LeafKey, &mut GridSector)> {
        let key = [SECTOR_LEVEL, 3, 4, 1, 0]
            .into_iter()
            .map(|level| LeafKey::containing(level, click))
            .find(|key| self.contains(*key))?;
        self.get_mut(key).map(|sector| (key, sector))
    }

    /// A stored leaf that overlaps `key` without being `key` itself: a
    /// coarser leaf that holds it, or a finer one inside it.
    pub fn conflict(&self, key: LeafKey) -> Option<LeafKey> {
        for level in 0..key.level {
            let ancestor = LeafKey::containing(level, key.min_click());
            if self.contains(ancestor) {
                return Some(ancestor);
            }
        }
        // Everything inside `key` has its corner in [m, m + n * n); the
        // leaf at `key` itself sorts first at `(m, key.level)`.
        let m = key.morton();
        let lo = (m, key.level.saturating_add(1));
        let hi = (m.saturating_add(key.morton_span()), 0);
        self.leaves
            .range(lo..hi)
            .map(|(_, leaf)| leaf.key)
            .find(|inner| inner.level > key.level && key.contains_click(inner.min_click()))
    }

    /// Store `sector` under `key`, replacing a leaf stored under exactly
    /// `key` (returned). Refuses a misaligned key and one that overlaps a
    /// different leaf, so leaves stay disjoint.
    pub fn insert(
        &mut self,
        key: LeafKey,
        sector: GridSector,
    ) -> Result<Option<GridSector>, LeafError> {
        if !key.is_aligned() {
            return Err(LeafError::Misaligned(key));
        }
        if !self.contains(key) {
            if let Some(other) = self.conflict(key) {
                return Err(LeafError::Overlaps(other));
            }
        }
        Ok(self
            .leaves
            .insert(key.order(), Leaf { key, sector })
            .map(|old| old.sector))
    }

    /// The leaf under `key`, created empty when absent. `Err` when `key` is
    /// not a valid place for a new leaf (see [`Self::insert`]).
    pub fn get_or_insert_with(
        &mut self,
        key: LeafKey,
        make: impl FnOnce() -> GridSector,
    ) -> Result<&mut GridSector, LeafError> {
        if !self.contains(key) {
            self.insert(key, make())?;
        }
        Ok(self
            .get_mut(key)
            .expect("the leaf was found or has just been stored"))
    }

    /// Remove and return the leaf stored under exactly `key`.
    pub fn remove(&mut self, key: LeafKey) -> Option<GridSector> {
        self.leaves.remove(&key.order()).map(|leaf| leaf.sector)
    }

    /// Every leaf in Morton order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (LeafKey, &GridSector)> + '_ {
        self.leaves.values().map(|leaf| (leaf.key, &leaf.sector))
    }

    /// Every leaf in Morton order, mutable.
    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (LeafKey, &mut GridSector)> + '_ {
        self.leaves
            .values_mut()
            .map(|leaf| (leaf.key, &mut leaf.sector))
    }

    /// Every key in Morton order.
    pub fn keys(&self) -> impl DoubleEndedIterator<Item = LeafKey> + '_ {
        self.leaves.values().map(|leaf| leaf.key)
    }

    /// The leaves that touch any world sector of the inclusive rectangle
    /// `lo..=hi`, in Morton order.
    pub fn in_sectors(
        &self,
        lo: [i32; 2],
        hi: [i32; 2],
    ) -> impl Iterator<Item = (LeafKey, &GridSector)> + '_ {
        self.iter().filter(move |(key, _)| {
            let (a, b) = key.sector_range();
            a[0] <= hi[0] && b[0] >= lo[0] && a[1] <= hi[1] && b[1] >= lo[1]
        })
    }

    /// Keep the leaves `keep` returns true for.
    pub fn retain(&mut self, mut keep: impl FnMut(LeafKey, &mut GridSector) -> bool) {
        self.leaves
            .retain(|_, leaf| keep(leaf.key, &mut leaf.sector));
    }

    /// Inclusive bounds in world sectors of everything stored, `(min, max)`,
    /// or `None` for an empty map.
    pub fn sector_bounds(&self) -> Option<([i32; 2], [i32; 2])> {
        self.keys().fold(None, |bounds, key| {
            let (lo, hi) = key.sector_range();
            Some(match bounds {
                None => (lo, hi),
                Some((a, b)) => (
                    [a[0].min(lo[0]), a[1].min(lo[1])],
                    [b[0].max(hi[0]), b[1].max(hi[1])],
                ),
            })
        })
    }

    /// Whether every leaf is a whole sector. Such a map is the dense
    /// sector list of every level made before the quadtree.
    pub fn is_sectors_only(&self) -> bool {
        self.keys().all(LeafKey::is_sector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(level: u8, x: i32, z: i32) -> LeafKey {
        LeafKey::new(level, x, z).unwrap()
    }

    #[test]
    fn leaves_are_found_by_key_and_by_click_at_every_level() {
        let mut map = LeafMap::new();
        map.insert(key(0, 0, 0), GridSector::default()).unwrap();
        map.insert(key(1, 16, 0), GridSector::default()).unwrap();
        map.insert(key(2, 24, 0), GridSector::default()).unwrap();
        map.insert(key(3, 28, 0), GridSector::default()).unwrap();
        map.insert(key(4, 30, 0), GridSector::default()).unwrap();
        assert_eq!(map.len(), 5);
        assert_eq!(map.leaf_at([15, 15]).unwrap().0, key(0, 0, 0));
        assert_eq!(map.leaf_at([23, 7]).unwrap().0, key(1, 16, 0));
        assert_eq!(map.leaf_at([27, 3]).unwrap().0, key(2, 24, 0));
        assert_eq!(map.leaf_at([29, 1]).unwrap().0, key(3, 28, 0));
        assert_eq!(map.leaf_at([30, 0]).unwrap().0, key(4, 30, 0));
        assert!(map.leaf_at([31, 0]).is_none());
        assert!(map.leaf_at([-1, 0]).is_none());
    }

    #[test]
    fn overlapping_and_misaligned_leaves_are_refused() {
        let mut map = LeafMap::new();
        map.insert(key(1, 0, 0), GridSector::default()).unwrap();
        // Inside the level-1 leaf, and a coarser leaf over it.
        assert_eq!(
            map.insert(key(2, 4, 4), GridSector::default()),
            Err(LeafError::Overlaps(key(1, 0, 0)))
        );
        assert_eq!(
            map.insert(key(0, 0, 0), GridSector::default()),
            Err(LeafError::Overlaps(key(1, 0, 0)))
        );
        // Neighbours are fine; replacing the very same key returns the old.
        assert!(map.insert(key(1, 8, 0), GridSector::default()).is_ok());
        assert_eq!(
            map.insert(key(1, 0, 0), GridSector::default()),
            Ok(Some(GridSector::default()))
        );
        let bad = LeafKey {
            level: 1,
            x: 4,
            z: 0,
        };
        assert_eq!(
            map.insert(bad, GridSector::default()),
            Err(LeafError::Misaligned(bad))
        );
    }

    #[test]
    fn iteration_is_morton_order_with_coarse_before_fine() {
        let mut map = LeafMap::new();
        for k in [
            key(2, 4, 0),
            key(2, 0, 4),
            key(2, 0, 0),
            key(2, 4, 4),
            key(2, -4, 0),
        ] {
            map.insert(k, GridSector::default()).unwrap();
        }
        let order: Vec<[i32; 2]> = map.keys().map(|k| [k.x, k.z]).collect();
        assert_eq!(order, [[-4, 0], [0, 0], [4, 0], [0, 4], [4, 4]]);
    }

    #[test]
    fn bounds_cover_coarse_leaves_whole() {
        let mut map = LeafMap::new();
        assert_eq!(map.sector_bounds(), None);
        map.insert(key(2, 4, 4), GridSector::default()).unwrap();
        assert_eq!(map.sector_bounds(), Some(([1, 1], [1, 1])));
        map.insert(key(0, 16, -16), GridSector::default()).unwrap();
        assert_eq!(map.sector_bounds(), Some(([1, -4], [7, 1])));
        assert!(!map.is_sectors_only());
    }

    #[test]
    fn a_random_quadtree_agrees_with_a_brute_force_lookup() {
        // Subdivide a 16 x 16 click block pseudo-randomly into leaves, then
        // check that every click resolves to the one leaf that covers it and
        // that no further leaf can be added anywhere.
        fn split(map: &mut LeafMap, key: LeafKey, seed: &mut u32) {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let divide = key.level < 4 && (*seed >> 16) % 3 != 0;
            match key.children().filter(|_| divide) {
                Some(children) => children.into_iter().for_each(|c| split(map, c, seed)),
                None => {
                    map.insert(key, GridSector::default()).unwrap();
                }
            }
        }
        for start in 0..8u32 {
            let mut seed = start;
            let mut map = LeafMap::new();
            split(&mut map, key(0, -16, 32), &mut seed);
            let mut covered = 0;
            for z in 32..48 {
                for x in -16..0 {
                    let hits: Vec<LeafKey> =
                        map.keys().filter(|k| k.contains_click([x, z])).collect();
                    assert_eq!(hits.len(), 1, "click ({x}, {z})");
                    assert_eq!(map.leaf_at([x, z]).map(|(k, _)| k), hits.first().copied());
                    covered += 1;
                }
            }
            assert_eq!(covered, 256);
            for k in map.keys().collect::<Vec<_>>() {
                assert!(map.conflict(k).is_none(), "{k:?}");
                if let Some(parent) = k.parent() {
                    assert!(map.conflict(parent).is_some(), "{parent:?}");
                }
            }
            // The Morton order is the order of a sort by corner bits.
            let keys: Vec<LeafKey> = map.keys().collect();
            let mut sorted = keys.clone();
            sorted.sort_by_key(|k| k.order());
            assert_eq!(keys, sorted);
        }
    }
}
