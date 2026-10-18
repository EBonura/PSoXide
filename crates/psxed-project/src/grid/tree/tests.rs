//! Property tests for the quadtree operations: random sequences of
//! subdivide, merge and height edits keep the tree sound and never change
//! what the room stands on.

use std::collections::BTreeMap;

use super::super::leaf::{LeafKey, CLICKS_PER_SECTOR};
use super::super::room::GridRoom;
use super::super::sector::GridSector;
use super::super::surface::{GridSplit, GridSurface};
use super::neighbours::{across, SIDES};
use super::normalize::{check, normalize, side_corners, TreeIssue};
use super::ops::{can_merge, merge, merge_all, subdivide, subdivide_to};
use super::surface_split::height_in_leaf;

/// A small deterministic generator.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 8
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
}

fn planar(rng: &mut Rng, base: i32) -> GridSurface {
    // A plane through four corners: base + a * x + b * z, steps of 64 gu.
    let a = rng.below(5) as i32 - 2;
    let b = rng.below(5) as i32 - 2;
    let h = |x: i32, z: i32| base + 64 * (a * x + b * z);
    let mut s = GridSurface::flat(0, None);
    s.heights = [h(0, 0), h(1, 0), h(1, 1), h(0, 1)];
    s.split = if rng.below(2) == 0 {
        GridSplit::NwSe
    } else {
        GridSplit::NeSw
    };
    s
}

/// An 8 by 8 sector room of planar sectors, origin on the 4-sector lattice.
fn room_of_planes(rng: &mut Rng) -> GridRoom {
    let mut room = GridRoom::new("r", 8, 8);
    for z in 0..8 {
        for x in 0..8 {
            let base = 256 * rng.below(4) as i32;
            let floor = planar(rng, base);
            let ceiling = GridSurface::flat(2048, None);
            room.set_sector(
                x,
                z,
                Some(GridSector::with_surfaces(Some(floor), Some(ceiling))),
            );
        }
    }
    room
}

/// Floor height at every click of the room's rectangle.
fn height_field(room: &GridRoom) -> BTreeMap<[i32; 2], Option<i32>> {
    let mut out = BTreeMap::new();
    for z in 0..room.depth as i32 * CLICKS_PER_SECTOR {
        for x in 0..room.width as i32 * CLICKS_PER_SECTOR {
            let click = [
                x + room.origin[0] * CLICKS_PER_SECTOR,
                z + room.origin[1] * CLICKS_PER_SECTOR,
            ];
            let h = room.leaf_at(click).and_then(|(key, sector)| {
                let floor = sector.ground_floor()?;
                Some(height_in_leaf(
                    floor,
                    click[0] - key.x,
                    click[1] - key.z,
                    key.edge_clicks(),
                ))
            });
            out.insert(click, h);
        }
    }
    out
}

fn assert_sound(room: &GridRoom, what: &str) {
    let issues = check(room);
    assert!(issues.is_empty(), "{what}: {issues:?}");
    // Every click resolves to exactly the leaf that covers it.
    for z in 0..room.depth as i32 * CLICKS_PER_SECTOR {
        for x in 0..room.width as i32 * CLICKS_PER_SECTOR {
            let click = [x + room.origin[0] * 4, z + room.origin[1] * 4];
            let hits: Vec<LeafKey> = room
                .leaves
                .keys()
                .filter(|k| k.contains_click(click))
                .collect();
            assert_eq!(hits.len(), 1, "{what}: click {click:?}");
            assert_eq!(room.leaf_at(click).map(|(k, _)| k), hits.first().copied());
        }
    }
    // Leaves are aligned and inside the room.
    for (key, _) in room.leaves() {
        assert!(
            key.is_aligned() && room.contains_leaf(key),
            "{what}: {key:?}"
        );
    }
}

fn some_leaf(room: &GridRoom, rng: &mut Rng) -> LeafKey {
    let keys: Vec<LeafKey> = room.leaves.keys().collect();
    keys[rng.below(keys.len() as u32) as usize]
}

#[test]
fn subdividing_never_changes_the_surface_and_merging_back_is_the_identity() {
    for seed in 0..12 {
        let mut rng = Rng(seed);
        let mut room = room_of_planes(&mut rng);
        let before = room.clone();
        let field = height_field(&room);
        // Subdivide a random sector twice, then merge everything back.
        let key = some_leaf(&room, &mut rng);
        let children = subdivide(&mut room, key).unwrap();
        assert_sound(&room, "after one subdivision");
        assert_eq!(height_field(&room), field, "seed {seed}: look changed");
        let grandchildren = subdivide(&mut room, children[rng.below(4) as usize]).unwrap();
        assert_sound(&room, "after two");
        let now = height_field(&room);
        let diff: Vec<_> = now
            .iter()
            .filter(|(k, v)| field[*k] != **v)
            .map(|(k, v)| (k, field[k], v))
            .collect();
        assert!(
            diff.is_empty(),
            "seed {seed}: look changed (2) {diff:?} {key:?} {children:?} {grandchildren:?}"
        );
        // Subdividing a fine child needs its neighbours balanced, so the
        // way back is merge_all.
        let merged = merge_all(&mut room, None, 2);
        assert!(merged >= 2, "seed {seed}");
        let _ = grandchildren;
        assert_eq!(
            room.leaves, before.leaves,
            "seed {seed}: merge is not identity"
        );
        assert_eq!(room, before);
    }
}

#[test]
fn a_plane_over_the_room_merges_up_to_big_leaves_and_looks_the_same() {
    // One global plane, cut into sectors.
    let h = |x: i32, z: i32| 512 + 64 * (x - 2 * z);
    let mut room = GridRoom::new("r", 8, 8);
    for z in 0..8i32 {
        for x in 0..8i32 {
            let mut s = GridSurface::flat(0, None);
            s.heights = [h(x, z), h(x + 1, z), h(x + 1, z + 1), h(x, z + 1)];
            room.set_sector(
                x as usize,
                z as usize,
                Some(GridSector::with_surfaces(Some(s), None)),
            );
        }
    }
    let field = height_field(&room);
    let merged = merge_all(&mut room, None, 0);
    assert!(merged > 0);
    assert_eq!(height_field(&room), field);
    assert_sound(&room, "merged plane");
    // The whole 8 by 8 room is four level-0 leaves.
    assert_eq!(room.leaf_count(), 4);
    assert!(room.leaves.keys().all(|k| k.level == 0));
    // And back down: subdividing every leaf to sectors restores the room.
    let keys: Vec<LeafKey> = room.leaves.keys().collect();
    super::ops::subdivide_all_to(&mut room, &keys, 2);
    assert_eq!(room.leaf_count(), 64);
    assert_eq!(height_field(&room), field);
}

#[test]
fn random_operations_keep_the_tree_balanced_planar_and_constrained() {
    for seed in 0..20 {
        let mut rng = Rng(100 + seed);
        let mut room = room_of_planes(&mut rng);
        for step in 0..60 {
            match rng.below(5) {
                0 | 1 => {
                    let key = some_leaf(&room, &mut rng);
                    let _ = subdivide(&mut room, key);
                }
                2 => {
                    let key = some_leaf(&room, &mut rng);
                    if let Some(parent) = key.parent() {
                        let _ = merge(&mut room, parent);
                    }
                }
                3 => {
                    // Raise one corner of a random leaf, the way a drag
                    // does, then restore the invariants.
                    let key = some_leaf(&room, &mut rng);
                    let corner = rng.below(4) as usize;
                    if let Some(s) = room.leaf_mut(key).and_then(|l| l.ground_floor_mut()) {
                        s.heights[corner] += 64;
                        if s.heights_b.is_some() {
                            s.heights_b = None;
                        }
                    }
                    normalize(&mut room);
                }
                _ => {
                    let click = [rng.below(32) as i32, rng.below(32) as i32];
                    let _ = subdivide_to(&mut room, click, 3 + rng.below(2) as u8);
                }
            }
            assert_sound(&room, &format!("seed {seed} step {step}"));
        }
        // Normalising a sound room changes nothing.
        let before = room.clone();
        let report = normalize(&mut room);
        assert_eq!((report.subdivided, report.corners), (0, 0));
        assert_eq!(room, before);
        // The room survives a save and load.
        let text = ron::to_string(&room).unwrap();
        let back: GridRoom = ron::from_str(&text).unwrap();
        assert_eq!(back, room);
    }
}

#[test]
fn constrained_corners_are_the_average_of_the_coarse_edge() {
    // One sector next to a subdivided one: the hanging corner on the shared
    // edge is the average of the sector's edge ends.
    let mut room = GridRoom::new("r", 4, 4);
    for z in 0..4 {
        for x in 0..4 {
            room.set_sector(
                x,
                z,
                Some(GridSector::with_surfaces(
                    Some(GridSurface::flat(0, None)),
                    None,
                )),
            );
        }
    }
    // Lift the corner shared by sectors (1,1), (2,1), (1,2), (2,2) by hand
    // and subdivide (2,1): its west edge now faces the coarse sector (1,1).
    for (x, z, c) in [(1, 1, 2), (2, 1, 3), (1, 2, 1), (2, 2, 0)] {
        room.sector_mut(x, z)
            .unwrap()
            .ground_floor_mut()
            .unwrap()
            .heights[c] = 256;
    }
    let key = LeafKey::sector([2, 1]);
    let children = subdivide(&mut room, key).unwrap();
    // Raise the free end of the fine west edge ... then the hanging corner
    // follows the average.
    let coarse = LeafKey::sector([1, 1]);
    let east = side_corners(super::super::sector::WallEdge::East);
    let (a, b) = {
        let f = room.leaf(coarse).unwrap().ground_floor().unwrap();
        (f.heights[east[0]], f.heights[east[1]])
    };
    let nw = room.leaf(children[0]).unwrap().ground_floor().unwrap();
    let sw = room.leaf(children[2]).unwrap().ground_floor().unwrap();
    // North-west child's SW corner is the hanging corner: the middle of the
    // coarse east edge.
    assert_eq!(nw.heights[3], (a + b) / 2);
    assert_eq!(sw.heights[0], (a + b) / 2);
    assert!(check(&room).is_empty());
}

#[test]
fn a_cliff_leaves_the_hanging_corner_free() {
    // Sectors (0,0) and (1,0) at different heights; subdividing (1,0) must
    // not pull its west corners to the coarse side's average.
    let mut room = GridRoom::new("r", 2, 1);
    room.set_sector(
        0,
        0,
        Some(GridSector::with_surfaces(
            Some(GridSurface::flat(0, None)),
            None,
        )),
    );
    room.set_sector(
        1,
        0,
        Some(GridSector::with_surfaces(
            Some(GridSurface::flat(512, None)),
            None,
        )),
    );
    let children = subdivide(&mut room, LeafKey::sector([1, 0])).unwrap();
    let nw = room.leaf(children[0]).unwrap().ground_floor().unwrap();
    assert_eq!(nw.heights, [512; 4]);
    assert!(check(&room).is_empty());
}

#[test]
fn balance_subdivides_a_neighbour_two_levels_coarser() {
    let mut room = GridRoom::new("r", 4, 4);
    for z in 0..4 {
        for x in 0..4 {
            room.set_sector(
                x,
                z,
                Some(GridSector::with_surfaces(
                    Some(GridSurface::flat(0, None)),
                    None,
                )),
            );
        }
    }
    // Sector (1,1) to level 3 forces nothing; one of its children to level
    // 4 makes the sector next to it unbalanced against level 2.
    let children = subdivide(&mut room, LeafKey::sector([1, 1])).unwrap();
    let east_child = children[1];
    let finest = subdivide(&mut room, east_child).unwrap();
    assert!(finest.iter().all(|k| k.level == 4));
    assert!(check(&room).is_empty(), "{:?}", check(&room));
    // The sector east of it was level 2 and is now level 3.
    let (east, _) = room.leaf_at([8, 4]).unwrap();
    assert_eq!(east.level, 3);
    for (key, _) in room.leaves() {
        for side in SIDES {
            for n in across(&room.leaves, key, side) {
                assert!(n.level.abs_diff(key.level) <= 1);
            }
        }
    }
}

#[test]
fn a_big_leaf_that_stops_being_planar_is_subdivided() {
    let mut room = GridRoom::new("r", 4, 4);
    for z in 0..4 {
        for x in 0..4 {
            room.set_sector(
                x,
                z,
                Some(GridSector::with_surfaces(
                    Some(GridSurface::flat(0, None)),
                    None,
                )),
            );
        }
    }
    assert!(merge_all(&mut room, None, 0) > 0);
    assert_eq!(room.leaf_count(), 1);
    let (key, _) = room.leaf_at([0, 0]).unwrap();
    assert_eq!(key.level, 0);
    room.leaf_mut(key)
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .heights[2] = 256;
    let report = normalize(&mut room);
    assert!(report.subdivided > 0);
    assert!(check(&room).is_empty());
    // The corner stays where it was put.
    let (corner, sector) = room.leaf_at([15, 15]).unwrap();
    assert!(corner.level > 0);
    assert_eq!(sector.ground_floor().unwrap().heights[2], 256);
}

#[test]
fn merge_refuses_what_the_parent_cannot_hold() {
    let mut room = GridRoom::new("r", 4, 4);
    for z in 0..4 {
        for x in 0..4 {
            room.set_sector(
                x,
                z,
                Some(GridSector::with_surfaces(
                    Some(GridSurface::flat(0, None)),
                    None,
                )),
            );
        }
    }
    let parent = LeafKey::containing(1, [0, 0]);
    assert!(can_merge(&room, parent).is_ok());
    // A raised corner makes the four sectors not one plane.
    room.sector_mut(0, 0)
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .heights[2] = 64;
    room.sector_mut(1, 0)
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .heights[3] = 64;
    room.sector_mut(0, 1)
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .heights[1] = 64;
    room.sector_mut(1, 1)
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .heights[0] = 64;
    assert!(can_merge(&room, parent).is_err());
    let _ = TreeIssue::BigLeafNotPlanar(parent);
}
