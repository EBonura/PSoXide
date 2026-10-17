//! Writer-side derivation of the sector summary flags
//! ([`SECTOR_FLAT_FLOOR`], [`SECTOR_FLAT_CEIL`], [`SECTOR_CLEAR`]).

use crate::format::*;

fn equal(h: [i16; 4]) -> bool {
    h[0] == h[1] && h[1] == h[2] && h[2] == h[3]
}

/// Set the summary bits of a room's dense sector table (`width * depth`,
/// z-major). Existing summary bits are recomputed, other bits are untouched.
/// A sector with an extension record (walls, planes or split level) is never
/// part of a clear neighbourhood.
pub fn summarize_sectors(sectors: &mut [Sector], width: usize, depth: usize) {
    let summary = SECTOR_FLAT_FLOOR | SECTOR_FLAT_CEIL | SECTOR_CLEAR;
    for sector in sectors.iter_mut() {
        let split = sector.flags & SECTOR_SPLIT_LEVEL != 0;
        let mut flags = sector.flags & !summary;
        if flags & SECTOR_HAS_FLOOR != 0 && !split && equal(sector.floor) {
            flags |= SECTOR_FLAT_FLOOR;
        }
        if flags & SECTOR_HAS_CEIL != 0 && !split && equal(sector.ceil) {
            flags |= SECTOR_FLAT_CEIL;
        }
        sector.flags = flags;
    }
    if sectors.len() != width * depth {
        return;
    }
    // CLEAR needs a full 3x3 neighbourhood inside the room.
    let mut clear = alloc::vec![false; sectors.len()];
    for z in 1..depth.saturating_sub(1) {
        for x in 1..width.saturating_sub(1) {
            clear[z * width + x] = neighbourhood_clear(sectors, width, x, z);
        }
    }
    for (sector, clear) in sectors.iter_mut().zip(clear) {
        if clear {
            sector.flags |= SECTOR_CLEAR;
        }
    }
}

fn neighbourhood_clear(sectors: &[Sector], width: usize, x: usize, z: usize) -> bool {
    let centre = &sectors[z * width + x];
    let floor = centre.floor[0];
    let solid = centre.flags & SECTOR_CEIL_SOLID != 0 && centre.flags & SECTOR_HAS_CEIL != 0;
    let ceil = centre.ceil[0];
    if solid && i32::from(ceil) - i32::from(floor) < CLEAR_HEADROOM {
        return false;
    }
    for nz in z - 1..=z + 1 {
        for nx in x - 1..=x + 1 {
            let s = &sectors[nz * width + nx];
            let n_solid = s.flags & SECTOR_CEIL_SOLID != 0 && s.flags & SECTOR_HAS_CEIL != 0;
            if s.ext != NO_EXT
                || s.flags & SECTOR_SPLIT_LEVEL != 0
                || s.flags & SECTOR_FLAT_FLOOR == 0
                || s.floor[0] != floor
                || n_solid != solid
                || (solid && (s.flags & SECTOR_FLAT_CEIL == 0 || s.ceil[0] != ceil))
            {
                return false;
            }
        }
    }
    true
}
