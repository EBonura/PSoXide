//! Cooking a room with leaves of several sizes: the render tables and the
//! collision records come from the leaves (T10), portals from the sector
//! view.

use super::mesh::open_edges;
use super::tests::{cook, rid};
use super::units::RuntimeUnits;
use super::*;
use crate::grid::leaf::LeafKey;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::tree::{merge_all, normalize, subdivide_to};
use crate::grid::units::GridScale;

/// An 8 by 8 derived hall, floor 0 and ceiling 2560, merged into big leaves.
fn big_hall() -> GridRoom {
    let mut room = GridRoom::new_derived("hall", 8, 8);
    for z in 0..8 {
        for x in 0..8 {
            let s = room.sector_or_insert(x, z).unwrap();
            s.set_ground_floor(Some(GridSurface::flat(0, Some(rid(1)))));
            s.set_ground_ceiling(Some(GridSurface::flat(2560, Some(rid(2)))));
        }
    }
    merge_all(&mut room, None, 0);
    room
}

fn world_of(room: GridRoom) -> GridWorld {
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn holes(world: &GridWorld) -> Vec<String> {
    holes_where(world, |_| true)
}

/// Open edges that are holes and satisfy `keep`.
fn holes_where(world: &GridWorld, keep: impl Fn(&super::mesh::OpenEdge) -> bool) -> Vec<String> {
    let cooked = cook(world).expect("the world cooks");
    open_edges(&cooked.tables[0])
        .iter()
        .filter(|e| e.hole && keep(e))
        .map(|e| format!("{:?} -> {:?}", e.a, e.b))
        .collect()
}

/// Holes not at a bow-tie cut: where two profiles cross inside a wall the
/// triangle's apex is the crossing, which is off the lattice of the leaves
/// (every other vertex of a room of default scale is a multiple of 5 runtime
/// units); the dense derivation has the same cut.
fn lattice_holes(world: &GridWorld) -> Vec<String> {
    holes_where(world, |e| e.a.iter().chain(e.b.iter()).all(|v| *v % 5 == 0))
}

/// Every floor and ceiling height the cooked collision of `a` and `b`
/// report on a grid of points over the first room, which must agree.
fn same_stands(a: &GridWorld, b: &GridWorld, step: usize) {
    let (na, nb) = (
        super::motor_harness::native(a),
        super::motor_harness::native(b),
    );
    let room = &a.rooms[0];
    let s = 80usize;
    let mut checked = 0;
    for z in (0..room.depth * s).step_by(step) {
        for x in (0..room.width * s).step_by(step) {
            let (cx, cz, lx, lz) = (x / s, z / s, (x % s) as u32, (z % s) as u32);
            assert_eq!(
                na.collision.floor_at(0, cx, cz, lx, lz),
                nb.collision.floor_at(0, cx, cz, lx, lz),
                "floor at ({x}, {z})"
            );
            assert_eq!(
                na.collision.ceil_at(0, cx, cz, lx, lz),
                nb.collision.ceil_at(0, cx, cz, lx, lz),
                "ceiling at ({x}, {z})"
            );
            checked += 1;
        }
    }
    assert!(checked > 100);
}

#[test]
fn a_hall_of_big_leaves_is_four_records_and_stands_like_the_sector_hall() {
    let big = world_of(big_hall());
    assert_eq!(big.rooms[0].leaf_count(), 4);
    let mut flat = big_hall();
    // The same hall as whole sectors.
    let keys: Vec<LeafKey> = flat.leaves.keys().collect();
    crate::grid::tree::subdivide_all_to(&mut flat, &keys, 2);
    let flat = world_of(flat);
    let (a, b) = (cook(&big).unwrap(), cook(&flat).unwrap());
    // One collision record per leaf, not per sector; the render tables are
    // those of the sector hall until the cook merges quads.
    assert_eq!(a.tables[0].sectors.len(), 4);
    assert_eq!(b.tables[0].sectors.len(), 64);
    assert!(a.tables[0].tree.is_some() && b.tables[0].tree.is_none());
    assert_eq!(a.tables[0].quads.len(), b.tables[0].quads.len());
    assert_eq!(a.tables[0].vertices, b.tables[0].vertices);
    assert!(a.report.warnings.is_empty(), "{:?}", a.report.warnings);
    assert!(holes(&big).is_empty());
    // Standing on the leaves is standing on the sectors, everywhere.
    same_stands(&big, &flat, 3);
    // And the core flags the room.
    assert_eq!(
        a.level.rooms[0].flags & psx_grid::ROOM_TREE,
        psx_grid::ROOM_TREE
    );
    assert_eq!(b.level.rooms[0].flags & psx_grid::ROOM_TREE, 0);
}

#[test]
fn a_subdivided_corner_with_fine_steps_cooks_without_a_hole() {
    let mut room = big_hall();
    // Fine steps in the north-west corner: quarter-sector treads.
    let keys: Vec<LeafKey> = room
        .leaves
        .in_sector([0, 0])
        .iter()
        .map(|(k, _)| *k)
        .collect();
    crate::grid::tree::subdivide_all_to(&mut room, &keys, 4);
    for i in 0..4 {
        let key = LeafKey::new(4, i, 0).unwrap();
        for z in 0..4 {
            let key = LeafKey::new(4, key.x, z).unwrap();
            if let Some(f) = room.leaf_mut(key).and_then(|s| s.ground_floor_mut()) {
                f.heights = [64 * (i + 1); 4];
            }
        }
    }
    normalize(&mut room);
    let world = world_of(room);
    let cooked = cook(&world).expect("a room with fine leaves cooks");
    assert!(holes(&world).is_empty(), "{:?}", holes(&world));
    // The steps cost quads: more than the plain hall.
    let plain = cook(&world_of(big_hall())).unwrap();
    assert!(cooked.tables[0].quads.len() > plain.tables[0].quads.len());
    // Collision is exact: no warning, one sub-block for the divided sector.
    assert!(
        cooked.report.warnings.is_empty(),
        "{:?}",
        cooked.report.warnings
    );
    let tree = cooked.tables[0].tree.as_ref().unwrap();
    assert!(tree.blocks.len() >= psx_grid::SUBBLOCK_ENTRIES);
    // Each tread is its own height: the floor under tread i is 64 (i + 1)
    // grid units, 5 runtime units per 64 at the default scale 5/4 per click.
    let native = super::motor_harness::native(&world);
    let units = RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    for i in 0..4i32 {
        let x = (i * 20 + 10) as u32;
        let want = units.rt(64 * (i + 1));
        assert_eq!(
            native.collision.floor_at(0, 0, 0, x, 30),
            Some(want),
            "tread {i}"
        );
    }
}

#[test]
fn a_planar_detail_inside_a_sector_is_collided_exactly() {
    // One sector subdivided but not changed: collision is exact (no warning)
    // and the sector stands as it did.
    let mut room = big_hall();
    subdivide_to(&mut room, [0, 0], 3).unwrap();
    let world = world_of(room);
    let cooked = cook(&world).unwrap();
    assert!(
        cooked.report.warnings.is_empty(),
        "{:?}",
        cooked.report.warnings
    );
    same_stands(&world, &world_of(big_hall()), 3);
}

/// A small deterministic generator.
struct Rng(u32);

impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) % n.max(1)
    }
}

/// A random mixed room: an 8 by 8 derived hall of planar sectors, then
/// random subdivisions, merges, lifts and (below a sector) cracks and voids.
fn random_room(seed: u32, features: bool) -> GridRoom {
    random_room_with(seed, features, false)
}

/// [`random_room`]; `calm` keeps every floor flat (no slopes, so no walls
/// whose profiles cross) for the checks that cannot tolerate a bow tie.
fn random_room_with(seed: u32, features: bool, calm: bool) -> GridRoom {
    use crate::grid::surface::{GridSplit, GridSurface};
    use crate::grid::tree::{merge, subdivide, subdivide_to};
    let mut rng = Rng(seed.wrapping_mul(7919).wrapping_add(13));
    let mut room = GridRoom::new_derived("random", 8, 8);
    for z in 0..8 {
        for x in 0..8 {
            let base = 64 * rng.below(8) as i32;
            let mut floor = GridSurface::flat(base, Some(rid(1)));
            let (a, b) = if calm {
                (0, 0)
            } else {
                (rng.below(3) as i32 - 1, rng.below(3) as i32 - 1)
            };
            floor.heights = [base, base + 64 * a, base + 64 * (a + b), base + 64 * b];
            floor.split = if rng.below(2) == 0 {
                GridSplit::NwSe
            } else {
                GridSplit::NeSw
            };
            let sector = room.sector_or_insert(x, z).unwrap();
            sector.set_ground_floor(Some(floor));
            sector.set_ground_ceiling(Some(GridSurface::flat(2560, Some(rid(2)))));
        }
    }
    for _ in 0..40 {
        let keys: Vec<LeafKey> = room.leaves.keys().collect();
        let key = keys[rng.below(keys.len() as u32) as usize];
        match rng.below(6) {
            0..=2 => {
                let _ = subdivide(&mut room, key);
            }
            3 => {
                if let Some(parent) = key.parent() {
                    let _ = merge(&mut room, parent);
                }
            }
            4 => {
                let click = [rng.below(32) as i32, rng.below(32) as i32];
                let _ = subdivide_to(&mut room, click, 3 + rng.below(2) as u8);
            }
            _ => {
                let corner = rng.below(4) as usize;
                if let Some(s) = room.leaf_mut(key).and_then(|l| l.ground_floor_mut()) {
                    if calm {
                        s.heights = s.heights.map(|h| h + 64);
                    } else {
                        s.heights[corner] += 64;
                    }
                    s.heights_b = None;
                }
                normalize(&mut room);
            }
        }
    }
    if features {
        // Cracks and voids on leaves of a sector or smaller.
        let keys: Vec<LeafKey> = room
            .leaves
            .keys()
            .filter(|k| k.level >= crate::grid::leaf::SECTOR_LEVEL)
            .collect();
        for key in keys {
            match rng.below(9) {
                0 => {
                    if let Some(f) = room.leaf_mut(key).and_then(|l| l.ground_floor_mut()) {
                        f.heights_b = Some(f.heights.map(|h| h + 64));
                    }
                }
                1 => {
                    // A void triangle is empty above as well as below.
                    let void = 1 + rng.below(2) as u8;
                    if let Some(l) = room.leaf_mut(key) {
                        let split = l.ground_floor().map(|f| f.split);
                        if let Some(f) = l.ground_floor_mut() {
                            f.void = void;
                        }
                        if let Some(c) = l.ground_ceiling_mut() {
                            c.void = void;
                            if let Some(split) = split {
                                c.split = split;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        normalize(&mut room);
    }
    room
}

/// What the document says the floor is at room-local runtime point
/// `(x, z)`, by the same rounded corners and height function the cook uses
/// but found through the leaf map, not the cooked index.
fn document_floor(room: &GridRoom, units: &RuntimeUnits, x: i32, z: i32) -> Option<i32> {
    use psx_grid::height::{height_at, in_first_triangle, weight_recip};
    let s = units.sector() as i32;
    let q = s / 4;
    let click = [
        room.origin[0] * 4 + x / q.max(1),
        room.origin[1] * 4 + z / q.max(1),
    ];
    let (key, sector) = room.leaf_at(click)?;
    let floor = sector.ground_floor()?;
    if !floor.walkable {
        return None;
    }
    let size = psx_grid::leaf_edge(s, super::leaf_collision::code_of(key.level));
    // The leaf's origin in room-local runtime units.
    let ox = (key.x - room.origin[0] * 4) * q;
    let oz = (key.z - room.origin[1] * 4) * q;
    let (lx, lz) = ((x - ox) as u32, (z - oz) as u32);
    let nesw = matches!(floor.split, crate::grid::surface::GridSplit::NeSw);
    let local =
        |g: [i32; 4]| g.map(|h| super::collision::local_height(units, room.y_offset, h) as i16);
    let second = !in_first_triangle(nesw, lx, lz, size as u32);
    let heights = match (second, floor.heights_b) {
        (true, Some(b)) => local(b),
        _ => local(floor.heights),
    };
    Some(height_at(
        heights,
        nesw,
        lx,
        lz,
        size as u32,
        weight_recip(size as u32),
    ))
}

#[test]
fn random_mixed_rooms_cook_to_what_the_document_says_at_every_point() {
    let units = RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    for seed in 0..24 {
        let room = random_room(seed, seed % 2 == 1);
        assert!(crate::grid::tree::check(&room).is_empty(), "seed {seed}");
        let world = world_of(room.clone());
        let cooked = cook(&world).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert!(
            cooked.report.warnings.is_empty(),
            "seed {seed}: {:?}",
            cooked.report.warnings
        );
        let native = super::motor_harness::native(&world);
        let s = 80;
        let mut mixed = false;
        for z in (0..8 * s).step_by(3) {
            for x in (0..8 * s).step_by(3) {
                let want = document_floor(&room, &units, x, z);
                let got = native.collision.floor_at(
                    0,
                    (x / s) as usize,
                    (z / s) as usize,
                    (x % s) as u32,
                    (z % s) as u32,
                );
                assert_eq!(got, want, "seed {seed} at ({x}, {z})");
                mixed |= want.is_some();
            }
        }
        assert!(mixed);
        assert!(
            lattice_holes(&world).is_empty(),
            "seed {seed}: {:?}",
            lattice_holes(&world)
        );
    }
}

/// Bands on every derived wall of a room of random leaves: faces stay
/// watertight.
#[test]
fn bands_in_mixed_rooms_leave_no_hole() {
    use crate::grid::bands::{Anchor, Band, BandSet};
    for seed in 0..16 {
        let mut room = random_room_with(100 + seed, false, true);
        room.band_sets = vec![BandSet {
            name: "Trim".to_string(),
            lines: vec![Anchor::Bottom(256), Anchor::Abs(1024)],
            bands: vec![Band::default(), Band::default(), Band::default()],
        }];
        room.defaults.bands = Some(0);
        let world = world_of(room);
        let found = holes(&world);
        assert!(found.is_empty(), "seed {seed}: {found:?}");
    }
}

#[test]
fn the_real_motor_climbs_fine_treads_one_step_at_a_time() {
    use psx_engine::RoomPoint;
    let mut room = big_hall();
    let keys: Vec<LeafKey> = room
        .leaves
        .in_sector([1, 1])
        .iter()
        .map(|(k, _)| *k)
        .collect();
    crate::grid::tree::subdivide_all_to(&mut room, &keys, 4);
    // Four treads across sector (1,1): clicks x 4..7, 64 grid units each.
    for i in 0..4 {
        for z in 4..8 {
            let key = LeafKey::new(4, 4 + i, z).unwrap();
            room.leaf_mut(key)
                .unwrap()
                .ground_floor_mut()
                .unwrap()
                .heights = [64 * (i + 1); 4];
        }
    }
    normalize(&mut room);
    let world = world_of(room);
    let native = super::motor_harness::native(&world);
    let units = RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    let tops: Vec<i32> = (1..=4).map(|i| units.rt(64 * i)).collect();
    // Start in the sector west of the treads, walking east.
    let path = super::motor_harness::walk_path(&native, RoomPoint::new(50, 2, 120), 4096, 0, 160);
    let ys: Vec<i32> = path.iter().map(|p| p.y).collect();
    let xs: Vec<i32> = path.iter().map(|p| p.x).collect();
    assert!(xs.last().unwrap() > &(80 + 4 * 20), "{xs:?}");
    // Every tread height is stood on at some tick, exactly.
    for (i, top) in tops.iter().enumerate() {
        assert!(
            ys.contains(top),
            "tread {i} at {top} never stood on: {ys:?}"
        );
    }
    // And the climb is monotone up to the top tread (past it the floor
    // drops back to the hall).
    let top = ys.iter().position(|y| *y == tops[3]).unwrap();
    assert!(ys[..=top].windows(2).all(|w| w[1] >= w[0]), "{ys:?}");
    // The heights on the way are the treads, not a ramp between them.
    let mut seen: Vec<i32> = ys[..=top].to_vec();
    seen.dedup();
    assert_eq!(seen, vec![0, 5, 10, 15, 20], "{ys:?}");
}
