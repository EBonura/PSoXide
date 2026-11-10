//! Helpers the composite level verbs (Room, Corridor, Opening, Arcade,
//! Terrain, Bridge) share: footprints, body headroom, sector readiness and
//! the storey aim.

use std::collections::BTreeSet;

use psx_engine::WalkLimits;

use crate::grid::cook::RuntimeUnits;
use crate::grid::leaf::SECTOR_LEVEL;
use crate::grid::room::GridRoom;
use crate::grid::units::{FINE_UNITS, SECTOR_UNITS};
use crate::grid::world::GridWorld;

use super::super::fit::{split_to_fit, ClickRect};
use super::super::focus::{Focus, FocusRefusal};
use super::super::rects::CellRect;

/// The cells of an octagon: the rectangle `rect` with each corner cut by a
/// 45 degree chamfer `cut` cells deep. Integer only. A cell is kept when its
/// distance in cells from each corner, `dx + dz`, reaches `cut`.
pub fn octagon_cells(rect: CellRect, cut: i32) -> BTreeSet<[i32; 2]> {
    let cut = cut.max(0);
    rect.cells()
        .filter(|&[x, z]| {
            let dx = (x - rect.min[0]).min(rect.max[0] - x);
            let dz = (z - rect.min[1]).min(rect.max[1] - z);
            dx + dz >= cut
        })
        .collect()
}

/// The smallest multiple of the fine step, grid units, that is at least
/// the body height at this scale: the least headroom a character fits under.
pub fn body_headroom(world: &GridWorld) -> Option<i32> {
    let units = RuntimeUnits::new(world.scale).ok()?;
    let mut h = FINE_UNITS;
    while units.rt(h) < WalkLimits::DEFAULT.body_height {
        h += FINE_UNITS;
        if h > SECTOR_UNITS * 64 {
            return None;
        }
    }
    Some(h)
}

/// Bounding rectangle of a cell set.
pub fn bounds_of(cells: &BTreeSet<[i32; 2]>) -> Option<CellRect> {
    let first = *cells.iter().next()?;
    let mut r = CellRect {
        min: first,
        max: first,
    };
    for c in cells {
        r.min = [r.min[0].min(c[0]), r.min[1].min(c[1])];
        r.max = [r.max[0].max(c[0]), r.max[1].max(c[1])];
    }
    Some(r)
}

/// Make sure the whole sector `cell` of `room` is held by one level-2 leaf
/// when it is held by a bigger one (split to fit). Returns false when finer
/// leaves hold part of the sector (the verb must refuse) .
pub fn sector_ready(room: &mut GridRoom, cell: [i32; 2]) -> bool {
    if let Some([x, z]) = room.local(cell) {
        if room.sector(x, z).is_some() {
            return true;
        }
    }
    let rect = ClickRect::of_cells(SECTOR_LEVEL, cell, cell);
    let key = crate::grid::leaf::LeafKey::sector(cell);
    if room.leaves.conflict(key).is_none() {
        return true;
    }
    split_to_fit(room, rect, SECTOR_LEVEL);
    room.local(cell)
        .is_some_and(|[x, z]| room.sector(x, z).is_some())
}

/// Run `edit` with the storey open at `aim` standing in as the ground span
/// of every column of `cells` (a no-op aim for rooms with cells of several
/// sizes, which never hold several spans).
pub fn aimed<R>(
    world: &mut GridWorld,
    room: usize,
    cells: &BTreeSet<[i32; 2]>,
    aim: Option<i32>,
    edit: impl FnOnce(&mut GridWorld) -> R,
) -> Result<R, FocusRefusal> {
    let Some(r) = world.rooms.get(room) else {
        return Err(FocusRefusal::NoSuchRoom);
    };
    if !r.leaves.is_sectors_only() {
        return Ok(edit(world));
    }
    Focus::capture(world, room, cells.iter().copied(), aim)?
        .run(world, edit)
        .map(|a| a.value)
}

fn mix(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// A 32 bit hash of `(seed, op, position, salt)`: every random decision of
/// the composite verbs is a pure function of these, never a running
/// sequence, so the result does not depend on the order cells are visited
/// in. Integers only, the same on every host.
pub fn hash(seed: u32, op: u32, at: [i32; 2], salt: u32) -> u32 {
    let mut h = mix(seed ^ 0x9e37_79b9);
    h = mix(h ^ op.wrapping_mul(0x85eb_ca6b));
    h = mix(h ^ (at[0] as u32).wrapping_mul(0xc2b2_ae35));
    h = mix(h ^ (at[1] as u32).wrapping_mul(0x27d4_eb2f));
    mix(h ^ salt.wrapping_mul(0x1656_67b1))
}

/// Whether the decision passes at `permille` chances in a thousand.
pub fn chance(seed: u32, op: u32, at: [i32; 2], permille: u32) -> bool {
    hash(seed, op, at, 0) % 1000 < permille
}
