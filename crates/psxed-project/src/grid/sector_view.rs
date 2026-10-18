//! A room of leaves seen at the runtime's granularity: whole sectors.
//!
//! The runtime reads collision, portals and the light bake's line of sight
//! per sector (`docs/grid/level-format-and-runtime.md`); the leaf format
//! that lets it read smaller and larger cells is phase T10. Until then the
//! cook builds those tables from this **sector view** of the room:
//!
//! * a leaf larger than a sector becomes the sectors it covers, each the
//!   plane sampled at its own corners. Exact.
//! * four leaves that merge back into a sector (the tree's own merge: the
//!   parent reproduces them exactly) are that sector. Exact.
//! * any other sector that holds fine leaves becomes a sector through the
//!   heights at its four corners, with the split the heights ask for.
//!   **Approximate**: collision stands on one quad where the render shows the
//!   detail, and authored walls on the fine leaves are not collided. The
//!   sector is returned so the cook can say so.
//!
//! Rendering never goes through the view: faces come from the leaves
//! themselves (`leaf_faces.rs`).

use super::auto_split::best_split;
use super::leaf::{LeafKey, CLICKS_PER_SECTOR, SECTOR_LEVEL};
use super::room::GridRoom;
use super::sector::GridSector;
use super::surface::{GridSplit, GridSurface};
use super::tree::surface_split::height_in_leaf;
use super::tree::{merge_all_unbalanced, MergeBlock};

/// The sector view of `room`: a room of whole sectors, and the world
/// sectors whose collision it could not make exact.
pub fn sector_room(room: &GridRoom) -> (GridRoom, Vec<[i32; 2]>) {
    if room.leaves.is_sectors_only() {
        return (room.clone(), Vec::new());
    }
    // Merge everything that merges back into a sector, exactly.
    let mut work = room.clone();
    merge_all_unbalanced(&mut work, SECTOR_LEVEL);
    let mut out = work.clone();
    out.leaves.clear();
    let mut approximate: Vec<[i32; 2]> = Vec::new();
    // Leaves of a sector or larger first, then the sectors that hold fine
    // leaves.
    let mut fine_cells: std::collections::BTreeSet<[i32; 2]> = std::collections::BTreeSet::new();
    for (key, sector) in work.leaves() {
        if key.level > SECTOR_LEVEL {
            fine_cells.insert(key.cell());
            continue;
        }
        if key.level == SECTOR_LEVEL {
            let _ = out.leaves.insert(key, sector.clone());
            continue;
        }
        let (lo, hi) = key.sector_range();
        for z in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                let cell = LeafKey::sector([x, z]);
                let _ = out.leaves.insert(cell, clip_sector(sector, key, cell));
            }
        }
    }
    for cell in fine_cells {
        let key = LeafKey::sector(cell);
        let _ = out.leaves.insert(key, sample_sector(&work, cell));
        approximate.push(cell);
    }
    (out, approximate)
}

/// A world made of whole sectors, and the `(room, world sector)` pairs whose
/// collision it could not make exact.
pub type SectorWorld = (super::world::GridWorld, Vec<(usize, [i32; 2])>);

/// The world seen at sector granularity, with the sectors that could not be
/// made exact: `None` when every room is already whole sectors (use the
/// world as it is).
pub fn sector_world(world: &super::world::GridWorld) -> Option<SectorWorld> {
    let mut out: Option<super::world::GridWorld> = None;
    let mut approximate = Vec::new();
    for (index, room) in world.rooms.iter().enumerate() {
        if room.leaves.is_sectors_only() {
            continue;
        }
        let (view, cells) = sector_room(room);
        approximate.extend(cells.into_iter().map(|c| (index, c)));
        out.get_or_insert_with(|| world.clone()).rooms[index] = view;
    }
    out.map(|w| (w, approximate))
}

/// The part of a leaf above a sector over `cell`: its surfaces sampled at
/// the cell's corners.
fn clip_sector(sector: &GridSector, leaf: LeafKey, cell: LeafKey) -> GridSector {
    let mut out = sector.clone();
    let n = leaf.edge_clicks();
    let corner = |dx: i32, dz: i32| (cell.x + dx - leaf.x, cell.z + dz - leaf.z);
    for surface in out.surfaces_mut() {
        let source = surface.clone();
        let at = |dx, dz| {
            let (x, z) = corner(dx, dz);
            height_in_leaf(&source, x, z, n)
        };
        let s = CLICKS_PER_SECTOR;
        surface.heights = [at(0, 0), at(s, 0), at(s, s), at(0, s)];
        surface.heights_b = None;
    }
    out
}

/// The heights of the ground floor (or ceiling) at the four corners of world
/// sector `cell`, `[NW, NE, SE, SW]`, read from whatever leaves lie there: a
/// sector's own corners, a larger leaf's plane at the sector's corners, or
/// the corner of each of the fine leaves in it.
pub fn corner_heights(room: &GridRoom, cell: [i32; 2], ceiling: bool) -> Option<[i32; 4]> {
    let pick = |s: &GridSector| -> Option<GridSurface> {
        if ceiling {
            s.ground_ceiling()
        } else {
            s.ground_floor()
        }
        .cloned()
    };
    let base = [cell[0] * CLICKS_PER_SECTOR, cell[1] * CLICKS_PER_SECTOR];
    let s = CLICKS_PER_SECTOR;
    // The click just inside each corner, and the leaf there.
    let corners = [(0, 0), (s - 1, 0), (s - 1, s - 1), (0, s - 1)];
    let mut heights = [0; 4];
    let any = room
        .leaves
        .in_sector(cell)
        .into_iter()
        .find_map(|(_, l)| pick(l))?;
    for (i, (ix, iz)) in corners.into_iter().enumerate() {
        let click = [base[0] + ix, base[1] + iz];
        let (key, leaf) = room.leaf_at(click)?;
        let surface = pick(leaf).unwrap_or_else(|| any.clone());
        let n = key.edge_clicks();
        // Where this corner of the sector lies in the leaf, in clicks.
        let (dx, dz) = (
            base[0] + if i == 1 || i == 2 { s } else { 0 } - key.x,
            base[1] + if i == 2 || i == 3 { s } else { 0 } - key.z,
        );
        heights[i] = height_in_leaf(&surface, dx, dz, n);
    }
    Some(heights)
}

/// A sector made from the fine leaves in it: the ground span's surfaces at
/// the four corners of the sector.
fn sample_sector(room: &GridRoom, cell: [i32; 2]) -> GridSector {
    let leaves = room.leaves.in_sector(cell);
    let first = leaves.first().map(|(_, s)| *s);
    let mut out = GridSector::default();
    if let Some(first) = first {
        out.rough = first.rough;
        out.set_ground_wall_material(first.ground_wall_material());
    }
    let sample = |ceiling: bool| -> Option<GridSurface> {
        let pick = |s: &GridSector| -> Option<GridSurface> {
            if ceiling {
                s.ground_ceiling()
            } else {
                s.ground_floor()
            }
            .cloned()
        };
        let template = leaves.iter().find_map(|(_, s)| pick(s))?;
        let heights = corner_heights(room, cell, ceiling)?;
        let mut surface = GridSurface {
            heights,
            heights_b: None,
            void: 0,
            ..template
        };
        surface.split = best_split(heights, GridSplit::NwSe);
        Some(surface)
    };
    out.set_ground_floor(sample(false));
    out.set_ground_ceiling(sample(true));
    out
}

/// Why a merge to sector level was refused, for tools that explain it.
pub fn merge_block(room: &GridRoom, parent: LeafKey) -> Option<MergeBlock> {
    super::tree::can_merge(room, parent).err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::tree::{normalize, subdivide, subdivide_to};

    fn room() -> GridRoom {
        let mut room = GridRoom::new_derived("r", 8, 8);
        for z in 0..8 {
            for x in 0..8 {
                room.set_sector(
                    x,
                    z,
                    Some(GridSector::with_surfaces(
                        Some(GridSurface::flat(256, None)),
                        Some(GridSurface::flat(2048, None)),
                    )),
                );
            }
        }
        room
    }

    #[test]
    fn a_room_of_sectors_is_its_own_view() {
        let r = room();
        let (view, approx) = sector_room(&r);
        assert_eq!(view, r);
        assert!(approx.is_empty());
    }

    #[test]
    fn big_leaves_and_mergeable_fine_leaves_are_exact() {
        let mut r = room();
        merge_all_big(&mut r);
        subdivide_to(&mut r, [0, 0], 4).unwrap();
        let (view, approx) = sector_room(&r);
        assert!(approx.is_empty(), "{approx:?}");
        assert!(view.leaves.is_sectors_only());
        assert_eq!(view.sector_count(), 64);
        assert!(view
            .iter()
            .all(|(_, _, s)| s.ground_floor().unwrap().heights == [256; 4]));
    }

    fn merge_all_big(r: &mut GridRoom) {
        crate::grid::tree::merge_all(r, None, 0);
    }

    #[test]
    fn a_sector_with_a_raised_fine_corner_is_approximated_and_reported() {
        let mut r = room();
        let kids = subdivide(&mut r, LeafKey::sector([3, 3])).unwrap();
        // Raise the middle corner (shared by all four children).
        for (i, c) in [(0, 2), (1, 3), (2, 1), (3, 0)] {
            r.leaf_mut(kids[i])
                .unwrap()
                .ground_floor_mut()
                .unwrap()
                .heights[c] = 512;
        }
        normalize(&mut r);
        let (view, approx) = sector_room(&r);
        assert_eq!(approx, vec![[3, 3]]);
        // The view stands on the four corner heights; the middle bump is the
        // detail it cannot hold.
        let s = view.leaves.get(LeafKey::sector([3, 3])).unwrap();
        assert_eq!(s.ground_floor().unwrap().heights, [256; 4]);
    }
}
