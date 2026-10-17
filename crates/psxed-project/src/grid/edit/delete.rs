//! Deleting faces and whole sectors.

use std::collections::{BTreeMap, BTreeSet};

use super::super::world::GridWorld;
use super::refs::{FaceRef, FaceTarget, SurfaceKind};

/// Delete `faces`. Walls go from the highest stack index down so the
/// indices of the others stay valid while the batch runs. Rooms left with
/// empty margins are trimmed. Returns the number of faces removed.
pub fn delete_faces(world: &mut GridWorld, faces: &[FaceRef]) -> usize {
    let mut removed = 0;
    let mut touched: BTreeSet<usize> = BTreeSet::new();
    let mut walls: BTreeMap<(usize, [i32; 2], super::super::sector::WallEdge), Vec<usize>> =
        BTreeMap::new();
    for face in faces {
        match face.target {
            FaceTarget::Surface(kind) => {
                let Some(sector) = face.sector_mut(world) else {
                    continue;
                };
                let slot = match kind {
                    SurfaceKind::Floor => &mut sector.floor,
                    SurfaceKind::Ceiling => &mut sector.ceiling,
                };
                if slot.take().is_some() {
                    removed += 1;
                    touched.insert(face.room);
                }
            }
            FaceTarget::Wall { edge, index } => {
                walls
                    .entry((face.room, face.cell, edge))
                    .or_default()
                    .push(index);
            }
            // A derived wall cannot be removed, only hidden.
            FaceTarget::Derived { .. } => {
                if super::derived::set_derived_hidden(world, *face, true) {
                    removed += 1;
                }
            }
        }
    }
    for ((room, cell, edge), mut indices) in walls {
        indices.sort_unstable();
        indices.dedup();
        let Some(sector) = FaceRef::floor(room, cell).sector_mut(world) else {
            continue;
        };
        let stack = sector.walls_mut(edge);
        for index in indices.into_iter().rev() {
            if index < stack.len() {
                stack.remove(index);
                removed += 1;
                touched.insert(room);
            }
        }
    }
    for room in touched {
        if let Some(r) = world.rooms.get_mut(room) {
            r.trim_empty();
        }
    }
    removed
}

/// Empty every listed sector of everything it holds (the 2D Delete).
pub fn clear_cells(world: &mut GridWorld, cells: &[(usize, [i32; 2])]) -> usize {
    let mut cleared = 0;
    let mut touched: BTreeSet<usize> = BTreeSet::new();
    for (room, cell) in cells {
        let Some(r) = world.rooms.get_mut(*room) else {
            continue;
        };
        let Some([x, z]) = r.local(*cell) else {
            continue;
        };
        if let Some(index) = r.index(x, z) {
            if r.sectors[index].take().is_some_and(|s| !s.is_empty()) {
                cleared += 1;
                touched.insert(*room);
            }
        }
    }
    for room in touched {
        if let Some(r) = world.rooms.get_mut(room) {
            r.trim_empty();
        }
    }
    cleared
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::sector::WallEdge;
    use crate::grid::surface::{GridSurface, GridWall};
    use crate::grid::units::GridScale;

    fn world() -> GridWorld {
        let mut room = GridRoom::new("r", 3, 1);
        for x in 0..3 {
            let s = room.sector_or_insert(x, 0).unwrap();
            s.floor = Some(GridSurface::flat(0, None));
            for i in 0..3 {
                s.north.push(GridWall::span(i * 256, i * 256 + 256, None));
            }
        }
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    #[test]
    fn deleting_several_walls_of_one_stack_keeps_the_right_ones() {
        let mut w = world();
        let n = |i| FaceRef::wall(0, [1, 0], WallEdge::North, i);
        // Given in ascending order: removing 0 first would shift the rest.
        let removed = delete_faces(&mut w, &[n(0), n(2)]);
        assert_eq!(removed, 2);
        let stack = &w.rooms[0].sector(1, 0).unwrap().north;
        assert_eq!(stack.len(), 1);
        assert_eq!(stack[0].heights, [256, 256, 512, 512]);
    }

    #[test]
    fn deleting_surfaces_and_stale_refs() {
        let mut w = world();
        let removed = delete_faces(
            &mut w,
            &[
                FaceRef::floor(0, [0, 0]),
                FaceRef::floor(0, [0, 0]),
                FaceRef::ceiling(0, [0, 0]),
                FaceRef::wall(0, [0, 0], WallEdge::East, 0),
            ],
        );
        assert_eq!(removed, 1);
        assert!(w.rooms[0].sector(0, 0).unwrap().floor.is_none());
    }

    #[test]
    fn emptied_margins_are_trimmed_without_moving_geometry() {
        let mut w = world();
        let all: Vec<FaceRef> = super::super::selection::all_faces_of_room(&w, 0)
            .into_iter()
            .filter(|f| f.cell[0] < 2)
            .collect();
        delete_faces(&mut w, &all);
        let room = &w.rooms[0];
        assert_eq!((room.origin, room.width), ([2, 0], 1));
    }

    #[test]
    fn clearing_cells_removes_everything_in_them() {
        let mut w = world();
        assert_eq!(clear_cells(&mut w, &[(0, [1, 0]), (0, [9, 9])]), 1);
        let room = &w.rooms[0];
        assert!(room
            .local([1, 0])
            .is_none_or(|[x, z]| room.sector(x, z).is_none()));
        assert_eq!(room.sector_count(), 2);
    }
}
