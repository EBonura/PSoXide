//! Leaf keys: where a quadtree leaf sits on the click lattice.
//!
//! The room is a linear quadtree whose leaves are keyed by `(level, x, z)` in
//! **world click units** (`docs/grid/level-tools-design.md` section 3). A
//! click is a quarter sector, the finest leaf. Level 2 is exactly today's
//! sector, so every level made before the quadtree is "all leaves at level
//! 2".
//!
//! | level | edge in clicks | edge in sectors |
//! | --- | --- | --- |
//! | 0 | 16 | 4 |
//! | 1 | 8 | 2 |
//! | 2 | 4 | 1 (a sector) |
//! | 3 | 2 | 1/2 |
//! | 4 | 1 | 1/4 |
//!
//! A leaf's corner is aligned to its own edge in **world** coordinates, never
//! room-local ones, so growing or trimming a room never moves a leaf (the
//! guarantee `GridRoom::grow_to_include` already gives sectors), and the
//! level-0 blocks are the world-anchored 4-sector lattice the streaming unit
//! planner cuts on (`docs/grid/streaming-design.md` 4.1 item 2).
//!
//! Fewest bits (`docs/grid/compact-format.md` section 2). The key is a
//! document type held in host memory and RON; the cooked tables will carry
//! room-relative values. Level needs 3 bits (0 to 4) and is `u8`; a
//! coordinate needs 12 signed bits over the editor extent of +-409 sectors
//! (+-1636 clicks) and is `i32`, the type of every other document position.
//! Inside a room of at most 128 sectors a click offset is 9 bits (512
//! clicks), which is what the leaf format of T10 stores.

use serde::{Deserialize, Serialize};

/// Number of leaf levels: 0 (four sectors across) down to 4 (a quarter).
pub const LEVELS: u8 = 5;
/// The level whose leaf is one sector.
pub const SECTOR_LEVEL: u8 = 2;
/// The finest level, a single click.
pub const FINEST_LEVEL: u8 = LEVELS - 1;
/// Clicks along a sector edge.
pub const CLICKS_PER_SECTOR: i32 = 4;
/// Clicks along a level-0 leaf, the world-anchored block of four sectors.
pub const BLOCK_CLICKS: i32 = 16;

/// Edge of a leaf of `level`, in clicks (`16 >> level`).
pub const fn edge_clicks(level: u8) -> i32 {
    BLOCK_CLICKS >> level
}

/// The place of a leaf in a room's quadtree: its level and its minimum
/// corner in world clicks.
///
/// The derived order is `(level, x, z)`; it is the order references to
/// leaves sort in (for level-2 leaves it equals the order of the sector
/// `[x, z]`). The order a [`super::leaf_map::LeafMap`] iterates in is
/// Morton order, see [`LeafKey::order`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeafKey {
    /// 0 (four sectors across) to 4 (a quarter sector).
    pub level: u8,
    /// World click of the minimum corner along X.
    pub x: i32,
    /// World click of the minimum corner along Z.
    pub z: i32,
}

/// The position of a leaf in Morton order: the interleaved bits of its
/// corner, with the level as the tiebreak (a coarser leaf sorts before the
/// finer leaves that start at the same corner).
pub type LeafOrder = (u64, u8);

/// Spread the 32 bits of `v` to the even bits of a `u64`.
fn spread(v: u32) -> u64 {
    let mut v = u64::from(v);
    v = (v | v << 16) & 0x0000_ffff_0000_ffff;
    v = (v | v << 8) & 0x00ff_00ff_00ff_00ff;
    v = (v | v << 4) & 0x0f0f_0f0f_0f0f_0f0f;
    v = (v | v << 2) & 0x3333_3333_3333_3333;
    (v | v << 1) & 0x5555_5555_5555_5555
}

/// Bias a signed click into an unsigned one that sorts the same way.
const fn bias(click: i32) -> u32 {
    (click as u32) ^ 0x8000_0000
}

impl LeafKey {
    /// The leaf of `level` at `(x, z)`, or `None` when the level is out of
    /// range or the corner is not aligned to the leaf's edge.
    pub const fn new(level: u8, x: i32, z: i32) -> Option<Self> {
        if level >= LEVELS {
            return None;
        }
        let key = Self { level, x, z };
        if key.is_aligned() {
            Some(key)
        } else {
            None
        }
    }

    /// The level-2 leaf (a whole sector) of world sector `cell`.
    pub const fn sector(cell: [i32; 2]) -> Self {
        Self {
            level: SECTOR_LEVEL,
            x: cell[0].saturating_mul(CLICKS_PER_SECTOR),
            z: cell[1].saturating_mul(CLICKS_PER_SECTOR),
        }
    }

    /// The leaf of `level` that contains the click `click`. `level` is
    /// clamped to the finest.
    pub const fn containing(level: u8, click: [i32; 2]) -> Self {
        let level = if level >= LEVELS { FINEST_LEVEL } else { level };
        let mask = !(edge_clicks(level) - 1);
        Self {
            level,
            x: click[0] & mask,
            z: click[1] & mask,
        }
    }

    /// Edge length in clicks.
    pub const fn edge_clicks(self) -> i32 {
        edge_clicks(self.level)
    }

    /// Whether the level is in range and the corner lies on the leaf's own
    /// lattice.
    pub const fn is_aligned(self) -> bool {
        if self.level >= LEVELS {
            return false;
        }
        let mask = edge_clicks(self.level) - 1;
        self.x & mask == 0 && self.z & mask == 0
    }

    /// Whether this is a whole sector (level 2).
    pub const fn is_sector(self) -> bool {
        self.level == SECTOR_LEVEL
    }

    /// The world sector `[x, z]` that holds the leaf's minimum corner. For a
    /// level-2 leaf this is the sector itself; a coarser leaf starts at its
    /// north-west sector, a finer one lies inside this one.
    pub const fn cell(self) -> [i32; 2] {
        [
            self.x.div_euclid(CLICKS_PER_SECTOR),
            self.z.div_euclid(CLICKS_PER_SECTOR),
        ]
    }

    /// Minimum corner in clicks.
    pub const fn min_click(self) -> [i32; 2] {
        [self.x, self.z]
    }

    /// One past the maximum corner in clicks.
    pub const fn end_click(self) -> [i32; 2] {
        let n = self.edge_clicks();
        [self.x + n, self.z + n]
    }

    /// Whether the leaf covers the click `click`.
    pub const fn contains_click(self, click: [i32; 2]) -> bool {
        let n = self.edge_clicks();
        click[0] >= self.x && click[0] < self.x + n && click[1] >= self.z && click[1] < self.z + n
    }

    /// Inclusive range of world sectors the leaf touches, `(min, max)`. A
    /// leaf finer than a sector touches one; a coarser one touches `n * n`.
    pub const fn sector_range(self) -> ([i32; 2], [i32; 2]) {
        let [x1, z1] = self.end_click();
        (
            self.cell(),
            [
                (x1 - 1).div_euclid(CLICKS_PER_SECTOR),
                (z1 - 1).div_euclid(CLICKS_PER_SECTOR),
            ],
        )
    }

    /// Whether the leaf covers any part of world sector `cell`.
    pub const fn touches_sector(self, cell: [i32; 2]) -> bool {
        let (lo, hi) = self.sector_range();
        cell[0] >= lo[0] && cell[0] <= hi[0] && cell[1] >= lo[1] && cell[1] <= hi[1]
    }

    /// The leaf one level up that holds this one, `None` at level 0.
    pub const fn parent(self) -> Option<Self> {
        if self.level == 0 {
            None
        } else {
            Some(Self::containing(self.level - 1, [self.x, self.z]))
        }
    }

    /// The four leaves one level down, in Morton order (north-west,
    /// north-east, south-west, south-east), `None` at the finest level.
    pub const fn children(self) -> Option<[Self; 4]> {
        if self.level >= FINEST_LEVEL {
            return None;
        }
        let level = self.level + 1;
        let h = edge_clicks(level);
        Some([
            Self {
                level,
                x: self.x,
                z: self.z,
            },
            Self {
                level,
                x: self.x + h,
                z: self.z,
            },
            Self {
                level,
                x: self.x,
                z: self.z + h,
            },
            Self {
                level,
                x: self.x + h,
                z: self.z + h,
            },
        ])
    }

    /// Where this leaf sits in Morton order. The interleaved corner bits
    /// are the same for a leaf and the first of its descendants, and a leaf
    /// covers exactly the half-open range `[m, m + n * n)` of them, which is
    /// what [`super::leaf_map::LeafMap`] uses to find overlaps.
    pub fn order(self) -> LeafOrder {
        (self.morton(), self.level)
    }

    /// The interleaved corner bits (x on the even bits, z on the odd ones),
    /// biased so negative clicks sort first.
    pub fn morton(self) -> u64 {
        spread(bias(self.x)) | spread(bias(self.z)) << 1
    }

    /// Number of Morton codes the leaf spans (`n * n`).
    pub const fn morton_span(self) -> u64 {
        let n = self.edge_clicks() as u64;
        n * n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_two_is_the_sector() {
        assert_eq!(edge_clicks(0), 16);
        assert_eq!(edge_clicks(SECTOR_LEVEL), CLICKS_PER_SECTOR);
        assert_eq!(edge_clicks(FINEST_LEVEL), 1);
        let key = LeafKey::sector([-3, 5]);
        assert_eq!((key.level, key.x, key.z), (2, -12, 20));
        assert!(key.is_sector() && key.is_aligned());
        assert_eq!(key.cell(), [-3, 5]);
        assert_eq!(key.sector_range(), ([-3, 5], [-3, 5]));
    }

    #[test]
    fn new_checks_alignment_and_level() {
        assert!(LeafKey::new(0, 16, -32).is_some());
        assert!(LeafKey::new(0, 4, 0).is_none());
        assert!(LeafKey::new(1, 8, 8).is_some());
        assert!(LeafKey::new(1, 4, 8).is_none());
        assert!(LeafKey::new(4, 3, -7).is_some());
        assert!(LeafKey::new(5, 0, 0).is_none());
    }

    #[test]
    fn containing_floors_towards_negative_infinity() {
        let k = LeafKey::containing(2, [-1, -1]);
        assert_eq!((k.x, k.z), (-4, -4));
        let k = LeafKey::containing(0, [-1, 17]);
        assert_eq!((k.x, k.z), (-16, 16));
        let k = LeafKey::containing(4, [5, -9]);
        assert_eq!((k.x, k.z), (5, -9));
        assert!(k.contains_click([5, -9]) && !k.contains_click([6, -9]));
    }

    #[test]
    fn parents_and_children_agree() {
        let leaf = LeafKey::new(2, 8, -4).unwrap();
        let parent = leaf.parent().unwrap();
        assert_eq!(parent, LeafKey::new(1, 8, -8).unwrap());
        assert!(parent.children().unwrap().contains(&leaf));
        for child in leaf.children().unwrap() {
            assert!(child.is_aligned());
            assert_eq!(child.parent(), Some(leaf));
        }
        assert_eq!(LeafKey::new(0, 0, 0).unwrap().parent(), None);
        assert_eq!(LeafKey::new(4, 0, 0).unwrap().children(), None);
    }

    #[test]
    fn coarse_leaves_touch_several_sectors() {
        let block = LeafKey::new(0, -16, 16).unwrap();
        assert_eq!(block.sector_range(), ([-4, 4], [-1, 7]));
        assert!(block.touches_sector([-1, 7]) && !block.touches_sector([0, 7]));
        let quarter = LeafKey::new(4, 5, 5).unwrap();
        assert_eq!(quarter.sector_range(), ([1, 1], [1, 1]));
    }

    #[test]
    fn morton_order_is_a_z_curve_and_children_sit_inside_the_parent_range() {
        let parent = LeafKey::new(1, 8, 8).unwrap();
        let (m, span) = (parent.morton(), parent.morton_span());
        for (i, child) in parent.children().unwrap().into_iter().enumerate() {
            assert!((m..m + span).contains(&child.morton()), "{child:?}");
            // North-west, north-east, south-west, south-east.
            if i > 0 {
                let prev = parent.children().unwrap()[i - 1];
                assert!(prev.morton() < child.morton());
            }
        }
        // Negative clicks sort before positive ones on each axis.
        let a = LeafKey::sector([-1, 0]).morton();
        let b = LeafKey::sector([0, 0]).morton();
        assert!(a < b);
        // A leaf sorts before the finer leaves that start at its corner.
        let coarse = LeafKey::new(2, 4, 4).unwrap();
        let fine = LeafKey::new(3, 4, 4).unwrap();
        assert!(coarse.order() < fine.order());
    }
}
