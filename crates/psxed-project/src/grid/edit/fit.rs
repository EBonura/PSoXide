//! Fitting the leaves of a room to a rectangle of clicks: the "split to fit"
//! every verb on cells of any size starts with.
//!
//! A rectangle of cells of one level is aligned to that level's lattice, so
//! subdividing the leaves that straddle it, or are coarser than its cells,
//! ends with every leaf the rectangle touches lying inside it and at least as
//! fine as a cell. Subdividing is lossless (`docs/grid/level-tools-design.md`
//! 3.5), so fitting never changes what the room looks like or stands on.

use super::super::leaf::{edge_clicks, LeafKey};
use super::super::room::GridRoom;
use super::super::tree::subdivide;

/// An inclusive rectangle of world clicks (quarter sectors).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClickRect {
    /// Smallest corner.
    pub min: [i32; 2],
    /// Largest corner, inclusive.
    pub max: [i32; 2],
}

impl ClickRect {
    /// The rectangle of the cells `lo..=hi` of `level` (cell coordinates at
    /// that level: `click / edge`).
    pub fn of_cells(level: u8, lo: [i32; 2], hi: [i32; 2]) -> Self {
        let e = edge_clicks(level);
        Self {
            min: [lo[0] * e, lo[1] * e],
            max: [hi[0] * e + e - 1, hi[1] * e + e - 1],
        }
    }

    /// Whether the leaf lies wholly inside.
    pub fn holds(&self, key: LeafKey) -> bool {
        let [x1, z1] = key.end_click();
        key.x >= self.min[0] && key.z >= self.min[1] && x1 - 1 <= self.max[0] && z1 - 1 <= self.max[1]
    }

    /// Whether the leaf overlaps.
    pub fn touches(&self, key: LeafKey) -> bool {
        let [x1, z1] = key.end_click();
        key.x <= self.max[0] && key.z <= self.max[1] && x1 > self.min[0] && z1 > self.min[1]
    }
}

/// Subdivide the leaves of `room` until every leaf touching `rect` lies
/// inside it and is of `level` or finer. Returns the leaves inside `rect`,
/// in map order. `rect` must be aligned to `level` cells (use
/// [`ClickRect::of_cells`]); an unaligned rectangle fits as far as the finest
/// leaf allows.
pub fn split_to_fit(room: &mut GridRoom, rect: ClickRect, level: u8) -> Vec<LeafKey> {
    loop {
        let cut: Vec<LeafKey> = room
            .leaves()
            .map(|(k, _)| k)
            .filter(|k| rect.touches(*k) && (!rect.holds(*k) || k.level < level))
            .collect();
        let mut progress = false;
        for key in cut {
            progress |= room.leaf(key).is_some() && subdivide(room, key).is_ok();
        }
        if !progress {
            break;
        }
    }
    room.leaves()
        .map(|(k, _)| k)
        .filter(|k| rect.holds(*k))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::GridSurface;

    fn hall() -> GridRoom {
        let mut room = GridRoom::new_derived("h", 4, 4);
        for z in 0..4 {
            for x in 0..4 {
                room.sector_or_insert(x, z)
                    .unwrap()
                    .set_ground_floor(Some(GridSurface::flat(0, None)));
            }
        }
        crate::grid::tree::merge_all(&mut room, None, 0);
        room
    }

    #[test]
    fn a_rectangle_of_quarter_cells_splits_the_big_leaf_down_to_them() {
        let mut room = hall();
        assert_eq!(room.leaf_count(), 1);
        let rect = ClickRect::of_cells(4, [5, 6], [6, 6]);
        let inside = split_to_fit(&mut room, rect, 4);
        assert_eq!(inside.len(), 2);
        assert!(inside.iter().all(|k| k.level == 4 && rect.holds(*k)));
        assert!(crate::grid::tree::check(&room).is_empty());
        // Splitting changed nothing about where the floor is.
        assert!(room
            .leaves()
            .all(|(_, s)| s.ground_floor().unwrap().heights == [0; 4]));
    }

    #[test]
    fn leaves_already_fine_enough_stay_and_coarser_ones_inside_split() {
        let mut room = hall();
        let rect = ClickRect::of_cells(2, [0, 0], [1, 0]);
        let inside = split_to_fit(&mut room, rect, 2);
        assert_eq!(inside.len(), 2);
        assert!(inside.iter().all(|k| k.level == 2));
        // A second call has nothing left to cut.
        let again = split_to_fit(&mut room, rect, 2);
        assert_eq!(again, inside);
    }
}
