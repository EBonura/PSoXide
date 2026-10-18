//! Which leaves lie across a side of a leaf.

use super::super::leaf::LeafKey;
use super::super::leaf_map::LeafMap;
use super::super::sector::WallEdge;

/// The four cardinal sides, in the order the tree code walks them.
pub const SIDES: [WallEdge; 4] = [
    WallEdge::North,
    WallEdge::East,
    WallEdge::South,
    WallEdge::West,
];

/// The side of the neighbour that faces `side`.
pub const fn facing(side: WallEdge) -> WallEdge {
    match side {
        WallEdge::North => WallEdge::South,
        WallEdge::East => WallEdge::West,
        WallEdge::South => WallEdge::North,
        _ => WallEdge::East,
    }
}

/// Whether `side` runs along X (north and south).
pub const fn runs_x(side: WallEdge) -> bool {
    matches!(side, WallEdge::North | WallEdge::South)
}

/// The position of `key`'s start along the axis a `side` runs on (west to
/// east for north and south, north to south for east and west), and its
/// length, in clicks.
pub fn along(key: LeafKey, side: WallEdge) -> (i32, i32) {
    let lo = if runs_x(side) { key.x } else { key.z };
    (lo, key.edge_clicks())
}

/// The click just outside `key` along `side`, `i` clicks from its start.
pub fn outside_click(key: LeafKey, side: WallEdge, i: i32) -> [i32; 2] {
    let n = key.edge_clicks();
    match side {
        WallEdge::North => [key.x + i, key.z - 1],
        WallEdge::South => [key.x + i, key.z + n],
        WallEdge::West => [key.x - 1, key.z + i],
        _ => [key.x + n, key.z + i],
    }
}

/// The leaves that touch `key` across `side`, nearest the start of the side
/// first, each once. Empty where the room has no leaf there.
pub fn across(map: &LeafMap, key: LeafKey, side: WallEdge) -> Vec<LeafKey> {
    let n = key.edge_clicks();
    let (start, _) = along(key, side);
    let mut out: Vec<LeafKey> = Vec::new();
    let mut i = 0;
    while i < n {
        match map.leaf_at(outside_click(key, side, i)) {
            Some((neighbour, _)) => {
                out.push(neighbour);
                let (lo, len) = along(neighbour, side);
                i = lo + len - start;
            }
            None => i += 1,
        }
    }
    out
}

/// Whether the room has a leaf finer than `key` across `side` (a hanging
/// vertex lies on that side).
pub fn has_finer(map: &LeafMap, key: LeafKey, side: WallEdge) -> bool {
    across(map, key, side).iter().any(|n| n.level > key.level)
}
