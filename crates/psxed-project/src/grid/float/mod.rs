//! Floating solids of a scanned model, as slabs.
//!
//! A level converted from a model keeps what stands on the ground in the
//! ground fit. What floats over it (a bridge, a gantry, a lintel) is not a
//! heightfield, and the fit cannot hold it. This module finds those solids in
//! the rays cast down the model ([`columns`]), covers them with rectangles
//! per room ([`pack`]) and places each rectangle as a stretched cube whose
//! collision is one thick platform ([`place`], `ImportProxy::Slab`).
//!
//! Heights come from the exact hits snapped to the 64 unit lattice, never
//! from voxel layers, so they do not depend on how the model was voxelised.

pub mod columns;
pub mod npy;
pub mod pack;
pub mod place;

use std::collections::HashMap;

use columns::{coarsen, PlanCell};
use pack::{components, pack, Component, Rect};
use place::{pieces, place, remove_placed, SlabStyle};

use super::mesh::MAX_MESHES_PER_ROOM;
use super::units::{CLICK_UNITS, SECTOR_UNITS};
use super::world::GridWorld;

/// What counts as floating and how the slabs are fitted.
#[derive(Clone, Debug)]
pub struct FloatConfig {
    /// Open space a solid needs under it to count as floating, grid units.
    /// Anything closer to the ground run belongs to the ground fit.
    pub clearance: i32,
    /// Largest gap that still joins two stacked floating solids.
    pub merge_gap: i32,
    /// Thinnest floating solid kept.
    pub min_thickness: i32,
    /// Thickest floating solid kept. Anything thicker is a mass the ground
    /// fit or a tower owns, not a deck.
    pub max_thickness: i32,
    /// Percent of a plan cell's rays that must agree on a solid.
    pub min_cover_percent: u32,
    /// Quarter-sector cells per side of a fitting block (4 is a sector).
    pub block_cells: i32,
    /// Percent of a block's cells that must agree on a solid.
    pub coarse_cover_percent: u32,
    /// Smallest patch kept, in fitting blocks.
    pub min_area_cells: usize,
    /// How far undersides and tops may differ inside one slab.
    pub tolerance: i32,
    /// World sector of the model's corner.
    pub origin_sector: [i32; 2],
}

impl Default for FloatConfig {
    fn default() -> Self {
        Self {
            clearance: 2304,
            merge_gap: 768,
            min_thickness: 512,
            max_thickness: 8192,
            min_cover_percent: 40,
            block_cells: 4,
            coarse_cover_percent: 50,
            min_area_cells: 1,
            tolerance: 768,
            origin_sector: [2, 2],
        }
    }
}

/// What one room received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomSlabs {
    /// Index of the room.
    pub room: usize,
    /// Its name.
    pub name: String,
    /// Instances placed.
    pub placed: usize,
    /// Plan cells they cover.
    pub cells: i32,
    /// Rectangles dropped because the room was full, smallest first.
    pub dropped: usize,
}

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FloatReport {
    /// Grid units along a fitting block.
    pub edge: i32,
    /// Every patch found, before the area filter.
    pub components: Vec<Component>,
    /// Cells in patches under the minimum area.
    pub small_cells: usize,
    /// Cells over a sector no room holds.
    pub unowned_cells: usize,
    /// Instances of an earlier run removed first.
    pub removed: usize,
    /// Per room that received slabs.
    pub rooms: Vec<RoomSlabs>,
    /// Slabs no cube stretches to.
    pub refused: Vec<String>,
}

impl FloatReport {
    /// Area of `blocks` fitting blocks, in square sectors.
    fn sectors2(&self, blocks: i32) -> f64 {
        f64::from(blocks) * f64::from(self.edge).powi(2) / f64::from(SECTOR_UNITS).powi(2)
    }

    /// Instances placed in all.
    pub fn placed(&self) -> usize {
        self.rooms.iter().map(|r| r.placed).sum()
    }

    /// A multi-line description for the converter's output.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        let kept = self.components.iter().filter(|c| c.cells >= 1).count();
        out += &format!("{kept} floating patch(es):\n");
        let mut order: Vec<&Component> = self.components.iter().collect();
        order.sort_by_key(|c| std::cmp::Reverse(c.cells));
        for c in order.iter().take(24) {
            out += &format!(
                "  {:7.2} sector2  x {}..{} z {}..{} (blocks)  clearance {:.1}  thick {:.1} sectors\n",
                self.sectors2(c.cells as i32),
                c.bounds.0[0],
                c.bounds.1[0],
                c.bounds.0[1],
                c.bounds.1[1],
                f64::from(c.mean_clearance) / 1024.0,
                f64::from(c.mean_thickness) / 1024.0,
            );
        }
        out += &format!(
            "dropped under the minimum area: {} block(s); over no room: {}; removed from an earlier run: {}\n",
            self.small_cells, self.unowned_cells, self.removed
        );
        out += &format!("{} slab(s) placed:\n", self.placed());
        for r in &self.rooms {
            out += &format!(
                "  room {} {:?}: {} slab(s), {:.2} sector2{}\n",
                r.room,
                r.name,
                r.placed,
                self.sectors2(r.cells),
                if r.dropped > 0 {
                    format!(", {} dropped (room full)", r.dropped)
                } else {
                    String::new()
                }
            );
        }
        for why in &self.refused {
            out += &format!("refused: {why}\n");
        }
        out
    }
}

/// The room that holds each world sector.
struct Owners(HashMap<[i32; 2], usize>);

impl Owners {
    fn of(world: &GridWorld) -> Self {
        let mut map = HashMap::new();
        for (index, room) in world.rooms.iter().enumerate() {
            for (key, _) in room.leaves() {
                let (lo, hi) = key.sector_range();
                for z in lo[1]..=hi[1] {
                    for x in lo[0]..=hi[0] {
                        map.entry([x, z]).or_insert(index);
                    }
                }
            }
        }
        Self(map)
    }
}

/// Place the floating solids `cells` describe into `world` as slabs, each in
/// the room that holds the sectors under it. Slabs of an earlier run are
/// removed first, so the run can be repeated.
///
/// # Errors
///
/// Returns a message when the mesh edit refuses a slab for a reason other
/// than a full room.
pub fn float_slabs(
    world: &mut GridWorld,
    cells: &[PlanCell],
    config: &FloatConfig,
    style: &SlabStyle,
) -> Result<FloatReport, String> {
    let mut report = FloatReport {
        removed: remove_placed(world),
        ..FloatReport::default()
    };
    let cells = &coarsen(cells, config.block_cells, config);
    let edge = config.block_cells.max(1) * CLICK_UNITS;
    report.edge = edge;
    let (labels, components) = components(cells);
    let owners = Owners::of(world);
    let origin = config.origin_sector.map(|s| s * SECTOR_UNITS);
    let mut by_room: HashMap<usize, Vec<PlanCell>> = HashMap::new();
    for (cell, &label) in cells.iter().zip(&labels) {
        if components[label].cells < config.min_area_cells {
            report.small_cells += 1;
            continue;
        }
        let sector = [
            (cell.x * edge).div_euclid(SECTOR_UNITS) + config.origin_sector[0],
            (cell.z * edge).div_euclid(SECTOR_UNITS) + config.origin_sector[1],
        ];
        match owners.0.get(&sector) {
            Some(&room) => by_room.entry(room).or_default().push(*cell),
            None => report.unowned_cells += 1,
        }
    }
    report.components = components;
    let mut rooms: Vec<usize> = by_room.keys().copied().collect();
    rooms.sort_unstable();
    for room in rooms {
        let mut rects = pack(&by_room[&room], config.tolerance);
        let mut planned: Vec<(Rect, Vec<place::Piece>)> = Vec::new();
        for rect in rects.drain(..) {
            match pieces(rect, edge) {
                Ok(p) => planned.push((rect, p)),
                Err(why) => report.refused.push(why),
            }
        }
        // Keep the largest when the room cannot take them all.
        planned.sort_by_key(|(rect, _)| std::cmp::Reverse(rect.area()));
        let capacity = MAX_MESHES_PER_ROOM.saturating_sub(world.rooms[room].meshes.len());
        let mut used = 0;
        let mut summary = RoomSlabs {
            room,
            name: world.rooms[room].name.clone(),
            placed: 0,
            cells: 0,
            dropped: 0,
        };
        for (rect, parts) in planned {
            if used + parts.len() > capacity {
                summary.dropped += 1;
                continue;
            }
            used += parts.len();
            for piece in parts {
                place(world, room, piece, origin, style)
                    .map_err(|e| format!("room {room}: {}", e.describe()))?;
                summary.placed += 1;
            }
            summary.cells += rect.area();
        }
        report.rooms.push(summary);
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
