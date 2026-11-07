//! Which sectors of its rectangle each room owns, for the level core.
//!
//! A room owns a sector when any of its leaves touches it. The runtime needs
//! that only where the rectangles of two rooms overlap and one of them is
//! empty over the overlap (an L-shaped room around another's corner, a hex
//! chunk beside its neighbours): there the rectangle would claim ground the
//! other room holds. A level without such a sector carries no table and
//! cooks exactly as before.

use crate::grid::room::GridRoom;
use crate::grid::world::GridWorld;

/// One bit per sector of each room's rectangle, row-major from local
/// `(0, 0)`, or an empty list when no room needs it.
pub fn derive(world: &GridWorld) -> Vec<Vec<u8>> {
    let owned: Vec<Vec<bool>> = world.rooms.iter().map(owned_cells).collect();
    let needed = world
        .rooms
        .iter()
        .zip(&owned)
        .enumerate()
        .any(|(i, (room, cells))| {
            cells.iter().enumerate().any(|(cell, &mine)| {
                !mine && {
                    let at = world_sector(room, cell);
                    world
                        .rooms
                        .iter()
                        .enumerate()
                        .any(|(j, other)| j != i && contains(other, at))
                }
            })
        });
    if !needed {
        return Vec::new();
    }
    owned
        .iter()
        .map(|cells| {
            let mut bits = vec![0u8; cells.len().div_ceil(8)];
            for (cell, _) in cells.iter().enumerate().filter(|(_, &mine)| mine) {
                bits[cell / 8] |= 1 << (cell % 8);
            }
            bits
        })
        .collect()
}

/// Whether each sector of the room's rectangle holds any leaf, row-major.
fn owned_cells(room: &GridRoom) -> Vec<bool> {
    (0..room.depth)
        .flat_map(|z| (0..room.width).map(move |x| (x, z)))
        .map(|(x, z)| !room.leaves_in_cell(x, z).is_empty())
        .collect()
}

/// World sector of the room's row-major cell index.
fn world_sector(room: &GridRoom, cell: usize) -> [i32; 2] {
    [
        room.origin[0] + (cell % room.width) as i32,
        room.origin[1] + (cell / room.width) as i32,
    ]
}

/// Whether world sector `at` lies inside the room's rectangle.
fn contains(room: &GridRoom, [x, z]: [i32; 2]) -> bool {
    let [ox, oz] = room.origin;
    x >= ox && z >= oz && x < ox + room.width as i32 && z < oz + room.depth as i32
}
