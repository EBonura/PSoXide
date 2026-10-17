//! The level verbs: Raise/Lower, Ramp, Stairs and Break.
//!
//! Every verb is a pure function pair over a [`GridWorld`](super::super::GridWorld)
//! with the contract `HeightDrag` already keeps: `begin` captures what the
//! verb may touch, and every `apply` first **restores that capture** and then
//! recomputes from absolute values, so a drag can be repeated, reversed or
//! cancelled exactly (apply a, apply b, apply 0 gives the document back byte
//! for byte). A verb writes only ordinary grid fields (corner heights, wall
//! materials, floor materials); nothing records which verb made them, and the
//! walls on the new edges are derived, so they show live.
//!
//! * [`break_verb`]: Break, seeded detail for walls, floors and ceilings.
//! * [`raise`]: Raise and Lower, in Offset and Level modes.
//! * [`ramp`]: a slope from one side of the region to the opposite side.
//! * [`stairs`]: a flight of whole-sector treads along a path.
//! * [`rules`]: the smart paint rules and the shared writing helpers.

pub mod break_verb;
pub mod raise;
pub mod ramp;
pub mod rules;
pub mod stairs;

use super::super::sector::GridSector;
use super::super::world::GridWorld;

/// What a verb application reports, for the status line: nothing is skipped
/// silently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Sectors whose floor moved.
    pub moved: usize,
    /// Region sectors that were skipped (no floor, or outside the room).
    pub skipped: usize,
    /// Floors held back one click under their ceiling.
    pub clamped: usize,
    /// Floors a verb made where there was none (Stairs).
    pub created: usize,
}

/// A copy of the sectors a verb may change, plus the room settings it may
/// write, taken when the gesture begins.
#[derive(Clone, Debug)]
pub struct Snapshot {
    room: usize,
    cells: Vec<([i32; 2], Option<GridSector>)>,
    band_sets: Vec<super::super::bands::BandSet>,
    defaults: super::super::overrides::RoomDefaults,
    edge_overrides: super::super::overrides::EdgeOverrides,
}

impl Snapshot {
    /// Capture `cells` (world sectors) of `room`. Cells outside the room are
    /// not captured and are never touched.
    pub fn capture(world: &GridWorld, room: usize, cells: &[[i32; 2]]) -> Option<Self> {
        let target = world.rooms.get(room)?;
        let cells = cells
            .iter()
            .filter_map(|&cell| {
                let [x, z] = target.local(cell)?;
                Some((cell, target.sector(x, z).cloned()))
            })
            .collect();
        Some(Self {
            room,
            cells,
            band_sets: target.band_sets.clone(),
            defaults: target.defaults,
            edge_overrides: target.edge_overrides.clone(),
        })
    }

    /// The room the snapshot belongs to.
    pub fn room(&self) -> usize {
        self.room
    }

    /// The captured cells and what they held.
    pub fn cells(&self) -> &[([i32; 2], Option<GridSector>)] {
        &self.cells
    }

    /// Put every captured sector and the room settings back.
    pub fn restore(&self, world: &mut GridWorld) {
        let Some(room) = world.rooms.get_mut(self.room) else {
            return;
        };
        for (cell, sector) in &self.cells {
            if let Some([x, z]) = room.local(*cell) {
                if let Some(index) = room.index(x, z) {
                    room.sectors[index] = sector.clone();
                }
            }
        }
        room.band_sets = self.band_sets.clone();
        room.defaults = self.defaults;
        room.edge_overrides = self.edge_overrides.clone();
    }
}

#[cfg(test)]
mod tests;
