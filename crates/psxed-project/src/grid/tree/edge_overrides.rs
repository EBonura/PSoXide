//! Keeping the room's edge overrides in step with the leaves.
//!
//! An override is addressed by the geometry of one wall segment (start in
//! world clicks, axis, length in clicks), and a segment is a run of the
//! finest common subdivision of the two leaves on either side of an edge. So
//! when a leaf splits, the overrides on its outer edges split in two, and
//! when four leaves join, equal halves join back. An override that cannot
//! be joined (the two halves differ, or one is missing) keeps the leaves
//! apart: merging them would lose what the user set.

use super::super::leaf::LeafKey;
use super::super::overrides::{Axis, EdgeKey, EdgeOverride};
use super::super::room::GridRoom;
use super::super::sector::WallEdge;
use super::surface_split::half;

/// The segment keys of `key`'s own edges, in the order `[north, east,
/// south, west, nw-se, ne-sw]`, the way [`super::super::derive::edge_key`]
/// addresses them at the leaf's own size.
pub fn leaf_edge_key(key: LeafKey, edge: WallEdge) -> EdgeKey {
    let n = key.edge_clicks();
    let len = n as u8;
    let (x, z, axis) = match edge {
        WallEdge::North => (key.x, key.z, Axis::X),
        WallEdge::South => (key.x, key.z + n, Axis::X),
        WallEdge::West => (key.x, key.z, Axis::Z),
        WallEdge::East => (key.x + n, key.z, Axis::Z),
        WallEdge::Nwse => (key.x, key.z, Axis::NwSe),
        WallEdge::Nesw => (key.x, key.z, Axis::NeSw),
    };
    EdgeKey { x, z, axis, len }
}

/// The two halves of a segment key, first the half nearer its start.
/// A diagonal is addressed by its sector's own corner, so its halves are
/// the two children the diagonal crosses.
fn halves(key: EdgeKey) -> [EdgeKey; 2] {
    let len = key.len / 2;
    let h = i32::from(len);
    let second = match key.axis {
        Axis::X => (key.x + h, key.z),
        Axis::Z => (key.x, key.z + h),
        Axis::NwSe => (key.x + h, key.z + h),
        // The diagonal starts at the north-east corner; its first half is
        // the north-east child, the second the south-west one.
        Axis::NeSw => (key.x, key.z + h),
    };
    let first = match key.axis {
        Axis::NeSw => (key.x + h, key.z),
        _ => (key.x, key.z),
    };
    [
        EdgeKey {
            x: first.0,
            z: first.1,
            len,
            ..key
        },
        EdgeKey {
            x: second.0,
            z: second.1,
            len,
            ..key
        },
    ]
}

fn split_override(o: &EdgeOverride) -> [EdgeOverride; 2] {
    let [a, b] = o.top_delta.map(i32::from);
    let m = half(a, b) as i16;
    [
        EdgeOverride {
            top_delta: [o.top_delta[0], m],
            ..o.clone()
        },
        EdgeOverride {
            top_delta: [m, o.top_delta[1]],
            ..o.clone()
        },
    ]
}

fn join_override(a: &EdgeOverride, b: &EdgeOverride) -> Option<EdgeOverride> {
    let joined = EdgeOverride {
        top_delta: [a.top_delta[0], b.top_delta[1]],
        ..a.clone()
    };
    let [x, y] = split_override(&joined);
    (x == *a && y == *b).then_some(joined)
}

/// Split the overrides on `key`'s outer edges for its four children.
pub fn split(room: &mut GridRoom, key: LeafKey) {
    if key.edge_clicks() < 2 {
        return;
    }
    for edge in WallEdge::ALL {
        let k = leaf_edge_key(key, edge);
        let Some(o) = room.edge_overrides.remove(&k) else {
            continue;
        };
        let [ka, kb] = halves(k);
        let [oa, ob] = split_override(&o);
        room.edge_overrides.edit(ka, |e| *e = oa);
        room.edge_overrides.edit(kb, |e| *e = ob);
    }
}

/// Whether the overrides around the four children of `parent` can be joined
/// for the parent: each outer edge holds equal halves or none, and no
/// override sits on an edge between two children.
pub fn can_join(room: &GridRoom, parent: LeafKey) -> bool {
    walk(room, parent, None)
}

/// Join the overrides of `parent`'s children. Call after [`can_join`].
pub fn join(room: &mut GridRoom, parent: LeafKey) {
    let mut pending = Vec::new();
    walk(room, parent, Some(&mut pending));
    for (halves, joined) in pending {
        for k in halves {
            room.edge_overrides.remove(&k);
        }
        room.edge_overrides.edit(joined.0, |e| *e = joined.1);
    }
}

type Joined = ([EdgeKey; 2], (EdgeKey, EdgeOverride));

fn walk(room: &GridRoom, parent: LeafKey, mut out: Option<&mut Vec<Joined>>) -> bool {
    let Some(children) = parent.children() else {
        return true;
    };
    let mut outer: Vec<EdgeKey> = Vec::new();
    for edge in WallEdge::ALL {
        let k = leaf_edge_key(parent, edge);
        let [ka, kb] = halves(k);
        outer.extend([ka, kb]);
        match (room.edge_overrides.get(&ka), room.edge_overrides.get(&kb)) {
            (None, None) => {}
            (Some(a), Some(b)) => match join_override(a, b) {
                Some(joined) => {
                    if let Some(out) = out.as_deref_mut() {
                        out.push(([ka, kb], (k, joined)));
                    }
                }
                None => return false,
            },
            _ => return false,
        }
    }
    // Anything else on a child's own edges lies between two children (or on
    // a diagonal the parent has no counterpart of).
    for child in children {
        for edge in WallEdge::ALL {
            let k = leaf_edge_key(child, edge);
            if !outer.contains(&k) && room.edge_overrides.get(&k).is_some() {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf() -> LeafKey {
        LeafKey::sector([2, 3])
    }

    #[test]
    fn an_override_on_an_outer_edge_splits_and_joins() {
        let mut room = GridRoom::new("r", 8, 8);
        let k = leaf_edge_key(leaf(), WallEdge::East);
        room.edge_overrides.edit(k, |o| {
            o.hidden = true;
            o.top_delta = [0, 64];
        });
        split(&mut room, leaf());
        assert!(room.edge_overrides.get(&k).is_none());
        assert_eq!(room.edge_overrides.len(), 2);
        let east_children = leaf().children().unwrap();
        let north_east = leaf_edge_key(east_children[1], WallEdge::East);
        assert_eq!(
            room.edge_overrides.get(&north_east).unwrap().top_delta,
            [0, 32]
        );
        assert!(can_join(&room, leaf()));
        join(&mut room, leaf());
        assert_eq!(room.edge_overrides.len(), 1);
        assert_eq!(room.edge_overrides.get(&k).unwrap().top_delta, [0, 64]);
    }

    #[test]
    fn unequal_halves_or_an_interior_override_block_the_join() {
        let mut room = GridRoom::new("r", 8, 8);
        room.edge_overrides
            .edit(leaf_edge_key(leaf(), WallEdge::North), |o| o.hidden = true);
        split(&mut room, leaf());
        let first = leaf().children().unwrap()[0];
        room.edge_overrides
            .edit(leaf_edge_key(first, WallEdge::North), |o| o.material = None);
        let second = leaf().children().unwrap()[1];
        room.edge_overrides
            .edit(leaf_edge_key(second, WallEdge::North), |o| o.hidden = false);
        assert!(!can_join(&room, leaf()), "one half is gone");
        // An override between two children blocks it too.
        let mut room = GridRoom::new("r", 8, 8);
        let [nw, _, _, _] = leaf().children().unwrap();
        room.edge_overrides
            .edit(leaf_edge_key(nw, WallEdge::East), |o| o.hidden = true);
        assert!(!can_join(&room, leaf()));
    }

    #[test]
    fn a_diagonal_override_goes_to_the_two_children_it_crosses() {
        let mut room = GridRoom::new("r", 8, 8);
        let k = leaf_edge_key(leaf(), WallEdge::Nesw);
        room.edge_overrides.edit(k, |o| o.hidden = true);
        split(&mut room, leaf());
        let c = leaf().children().unwrap();
        assert!(room
            .edge_overrides
            .get(&leaf_edge_key(c[1], WallEdge::Nesw))
            .is_some());
        assert!(room
            .edge_overrides
            .get(&leaf_edge_key(c[2], WallEdge::Nesw))
            .is_some());
        assert!(can_join(&room, leaf()));
    }
}
