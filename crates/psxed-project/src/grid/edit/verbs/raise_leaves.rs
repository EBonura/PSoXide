//! Raise and Lower in a room with leaves of several sizes.
//!
//! The region is still sectors. What moves is every leaf inside them: a
//! sector held by four smaller leaves moves all four (the shape of the
//! small steps is kept in Offset mode), and a leaf larger than a sector is
//! first subdivided down to sectors where the region reaches it, which is
//! lossless ("split to fit"), so a region of two sectors out of a
//! four-sector leaf raises those two and leaves the other two where they
//! were. The tree is normalised afterwards, so a neighbour's hanging
//! corners follow.
//!
//! Like every verb it restores what it captured before each application,
//! so a drag can be repeated or reversed: here the capture is the room's
//! whole leaf map.

use crate::grid::auto_split;
use crate::grid::leaf::{LeafKey, SECTOR_LEVEL};
use crate::grid::leaf_map::LeafMap;
use crate::grid::room::GridRoom;
use crate::grid::tree::{normalize, subdivide_all_to};

use super::raise::{RaiseMode, RaiseParams};
use super::rules::{
    ceiling_corners, clamp_under_ceiling, floor_corners, move_ceiling_with_floor,
    set_floor_corners, stamp_wall_material,
};
use super::Applied;

/// The leaves of the room when the drag began, and the sectors it acts on.
#[derive(Clone, Debug)]
pub(super) struct LeafRaise {
    leaves: LeafMap,
    cells: Vec<[i32; 2]>,
}

impl LeafRaise {
    pub(super) fn capture(room: &GridRoom, cells: &[[i32; 2]]) -> Self {
        Self {
            leaves: room.leaves.clone(),
            cells: cells.to_vec(),
        }
    }

    pub(super) fn apply(&self, room: &mut GridRoom, params: RaiseParams, amount: i32) -> Applied {
        room.leaves = self.leaves.clone();
        let mut applied = Applied::default();
        // Split to fit: leaves above a sector that the region reaches.
        let big: Vec<LeafKey> = self
            .cells
            .iter()
            .flat_map(|c| room.leaves.in_sector(*c))
            .map(|(k, _)| k)
            .filter(|k| k.level < SECTOR_LEVEL)
            .collect();
        if !big.is_empty() {
            subdivide_all_to(room, &big, SECTOR_LEVEL);
        }
        let mut units: Vec<LeafKey> = self
            .cells
            .iter()
            .flat_map(|c| room.leaves.in_sector(*c))
            .map(|(k, _)| k)
            .collect();
        units.sort_unstable();
        units.dedup();
        let rules = room.defaults.rules;
        for key in units {
            let Some(sector) = room.leaf_mut(key) else {
                continue;
            };
            let Some(old) = floor_corners(sector) else {
                applied.skipped += 1;
                continue;
            };
            let mut wanted = old;
            for tri in &mut wanted {
                for h in tri.iter_mut() {
                    *h = match params.mode {
                        RaiseMode::Offset => h.saturating_add(amount),
                        RaiseMode::Level => params.reference.saturating_add(amount),
                    };
                }
            }
            let mut held = false;
            if !params.follow_ceiling {
                if let Some(ceiling) = ceiling_corners(sector) {
                    let (clamped, was_held) = clamp_under_ceiling(ceiling, old, wanted);
                    wanted = clamped;
                    held = was_held;
                }
            }
            if let Some(floor) = sector.ground_floor_mut() {
                set_floor_corners(floor, wanted);
                if wanted != old {
                    auto_split::resplit(floor);
                }
            }
            if params.follow_ceiling {
                let deltas = [0, 1].map(|t| core::array::from_fn(|i| wanted[t][i] - old[t][i]));
                move_ceiling_with_floor(sector, deltas);
                if let Some(ceiling) = sector.ground_ceiling_mut() {
                    if deltas != [[0; 4]; 2] {
                        auto_split::resplit(ceiling);
                    }
                }
            }
            stamp_wall_material(sector, params.wall_material, &rules);
            applied.moved += usize::from(wanted != old);
            applied.clamped += usize::from(held);
        }
        normalize(room);
        applied
    }
}
