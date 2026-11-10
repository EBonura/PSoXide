//! Tests of the Terrain patch verb.

use std::collections::BTreeSet;

use crate::grid::edit::CellRect;
use crate::grid::walk::analyse;
use crate::grid::world::GridWorld;

use super::room::{lay_room, Footprint, LeafPolicy, RoomParams};
use super::room_tests::empty;
use super::terrain::{lay_terrain, Edge, TerrainParams, TerrainRefusal, MAX_EDGE_RISE};

fn ground() -> GridWorld {
    let mut w = empty();
    let mut p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [15, 15])),
        "yard",
        8192,
    );
    p.leaves = LeafPolicy::Sectors;
    lay_room(&mut w, &p).unwrap();
    w
}

fn cells(a: [i32; 2], b: [i32; 2]) -> BTreeSet<[i32; 2]> {
    CellRect::from_corners(a, b).cells().collect()
}

fn corner(w: &GridWorld, cell: [i32; 2], i: usize) -> i32 {
    let r = &w.rooms[0];
    let [x, z] = r.local(cell).unwrap();
    r.sector(x, z).unwrap().ground_floor().unwrap().heights[i]
}

fn rough(seed: u32) -> TerrainParams {
    let mut p = TerrainParams::new(0, cells([2, 2], [13, 13]), seed);
    p.amplitude_clicks = 8;
    p.edge = Edge::Cliff;
    p
}

#[test]
fn one_seed_gives_one_ground_and_another_seed_a_re_roll() {
    let (mut a, mut b, mut c) = (ground(), ground(), ground());
    lay_terrain(&mut a, &rough(7)).unwrap();
    lay_terrain(&mut b, &rough(7)).unwrap();
    lay_terrain(&mut c, &rough(8)).unwrap();
    assert!(a == b, "same seed, same ground");
    assert!(a != c, "another seed, another ground");
    let r = lay_terrain(&mut ground(), &rough(7)).unwrap();
    assert!(r.range.1 > r.range.0, "there is relief: {r:?}");
    assert_eq!(r.cells, 144);
}

#[test]
fn heights_are_on_the_fine_lattice_and_corners_are_welded() {
    let mut w = ground();
    lay_terrain(&mut w, &rough(3)).unwrap();
    for cell in cells([2, 2], [13, 13]) {
        for i in 0..4 {
            assert_eq!(corner(&w, cell, i) % 64, 0, "{cell:?} {i}");
        }
    }
    // The east corners of one cell are the west corners of the next.
    assert_eq!(corner(&w, [5, 5], 1), corner(&w, [6, 5], 0));
    assert_eq!(corner(&w, [5, 5], 2), corner(&w, [6, 5], 3));
}

#[test]
fn two_patches_with_one_seed_tile_at_their_border() {
    let mut w = ground();
    let mut left = rough(11);
    left.cells = cells([0, 0], [7, 7]);
    left.keep_walkable = false;
    let mut right = rough(11);
    right.keep_walkable = false;
    right.cells = cells([8, 0], [15, 7]);
    lay_terrain(&mut w, &left).unwrap();
    lay_terrain(&mut w, &right).unwrap();
    for z in 0..8 {
        assert_eq!(corner(&w, [7, z], 1), corner(&w, [8, z], 0), "row {z}");
    }
    // And equals one patch over both.
    let mut whole = ground();
    let mut p = rough(11);
    p.cells = cells([0, 0], [15, 7]);
    p.keep_walkable = false;
    lay_terrain(&mut whole, &p).unwrap();
    assert_eq!(corner(&w, [7, 3], 1), corner(&whole, [7, 3], 1));
}

#[test]
fn keep_walkable_holds_every_cell_under_the_slope_the_motor_stands_on() {
    let mut steep = rough(5);
    steep.amplitude_clicks = 24;
    steep.wavelength = 2;
    steep.keep_walkable = false;
    let mut w = ground();
    let r = lay_terrain(&mut w, &steep).unwrap();
    assert!(r.steepest > MAX_EDGE_RISE, "the raw field is steep: {r:?}");
    let a = analyse(&w, &[], &psx_engine::WalkLimits::DEFAULT).unwrap();
    let raw_steep = cells([2, 2], [13, 13])
        .into_iter()
        .filter(|c| a.rooms[0].cell(*c).is_some_and(|c| c.steep))
        .count();
    assert!(raw_steep > 0);
    let mut kept = steep.clone();
    kept.keep_walkable = true;
    let mut w = ground();
    let r = lay_terrain(&mut w, &kept).unwrap();
    assert!(r.steepest <= MAX_EDGE_RISE, "{r:?}");
    assert!(r.clamped > 0);
    let a = analyse(&w, &[], &psx_engine::WalkLimits::DEFAULT).unwrap();
    let steep_now = cells([2, 2], [13, 13])
        .into_iter()
        .filter(|c| a.rooms[0].cell(*c).is_some_and(|c| c.steep))
        .count();
    assert_eq!(steep_now, 0);
}

#[test]
fn the_edge_blend_meets_the_neighbours_and_the_ceiling_follows() {
    let mut w = ground();
    let mut p = rough(9);
    p.edge = Edge::Blend(3);
    lay_terrain(&mut w, &p).unwrap();
    // Corners on the patch border stay at the neighbours' height.
    assert_eq!(corner(&w, [2, 5], 0), 0);
    assert_eq!(corner(&w, [13, 5], 1), 0);
    assert_eq!(corner(&w, [5, 2], 0), 0);
    // Deeper in, the relief arrives.
    let inner: i32 = cells([6, 6], [9, 9])
        .iter()
        .map(|c| corner(&w, *c, 0).abs())
        .sum();
    assert!(inner > 0);
    // Headroom stays 8192 under every moved floor.
    let r = &w.rooms[0];
    for cell in cells([4, 4], [11, 11]) {
        let [x, z] = r.local(cell).unwrap();
        let s = r.sector(x, z).unwrap();
        let (f, c) = (s.ground_floor().unwrap(), s.ground_ceiling().unwrap());
        for i in 0..4 {
            assert_eq!(c.heights[i] - f.heights[i], 8192);
        }
    }
}

#[test]
fn a_cliff_edge_keeps_full_relief_and_refusals_change_nothing() {
    let mut w = ground();
    let before = w.clone();
    let mut p = rough(1);
    p.octaves = 9;
    assert_eq!(
        lay_terrain(&mut w, &p),
        Err(TerrainRefusal::BadParameter("octaves"))
    );
    p = rough(1);
    p.cells = BTreeSet::new();
    assert_eq!(lay_terrain(&mut w, &p), Err(TerrainRefusal::EmptyRegion));
    p = rough(1);
    p.cells = cells([20, 20], [21, 21]);
    assert!(matches!(
        lay_terrain(&mut w, &p),
        Err(TerrainRefusal::NoFloor { .. })
    ));
    p.room = 5;
    assert_eq!(lay_terrain(&mut w, &p), Err(TerrainRefusal::NoSuchRoom));
    assert!(w == before);
    // With create and a base the same cells get a floor.
    let mut p = rough(1);
    p.cells = cells([20, 20], [23, 23]);
    p.base = Some(512);
    p.create = true;
    let r = lay_terrain(&mut w, &p).unwrap();
    assert_eq!(r.created, 16);
}
