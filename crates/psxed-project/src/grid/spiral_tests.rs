//! Spiral stairs on a ring of sectors: the walk, the slabs it lays, the
//! second pass over a sector, and the exact drag.

use super::edit::verbs::spiral::{
    is_ring, lay_spiral, ring_cells, walk, SpiralDrag, SpiralParams, SpiralRefusal,
};
use super::room::GridRoom;
use super::sector::GridSector;
use super::storeys::storeys;
use super::surface::GridSurface;
use super::units::{GridScale, CLICK_UNITS};
use super::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A derived hall of `side` by `side` tall columns: floor 0, ceiling 48 clicks.
fn hall(side: usize) -> GridWorld {
    let mut room = GridRoom::new_derived("tower", side, side);
    for z in 0..side {
        for x in 0..side {
            *room.sector_or_insert(x, z).unwrap() = GridSector::with_surfaces(
                Some(GridSurface::flat(0, Some(rid(1)))),
                Some(GridSurface::flat(48 * CLICK_UNITS, Some(rid(2)))),
            );
        }
    }
    GridWorld {
        scale: GridScale { num: 1, den: 1 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// The corners of a square ring `side` sectors across.
fn square(side: i32) -> Vec<[i32; 2]> {
    let n = side - 1;
    vec![[0, 0], [n, 0], [n, n], [0, n]]
}

fn floors(world: &GridWorld, cell: [i32; 2]) -> Vec<i32> {
    let room = &world.rooms[0];
    let [x, z] = room.local(cell).unwrap();
    room.sector(x, z)
        .unwrap()
        .spans()
        .iter()
        .filter_map(|s| s.floor.as_ref().map(|f| f.heights[0] / CLICK_UNITS))
        .collect()
}

#[test]
fn waypoints_make_a_closed_ring_and_a_line_does_not() {
    let ring = ring_cells(&square(6)).expect("a square ring");
    assert_eq!(ring.len(), 20);
    assert!(is_ring(&ring));
    assert_eq!(ring[0], [0, 0]);
    // A straight run, or a ring that crosses itself, is no ring.
    assert!(ring_cells(&[[0, 0], [5, 0]]).is_none());
    assert!(!is_ring(&[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]));
    assert!(!is_ring(&[[0, 0], [2, 0], [2, 1], [0, 1]]));
}

#[test]
fn treads_climb_one_riser_and_turns_are_flat_landings() {
    let ring = ring_cells(&square(6)).unwrap();
    // Four sides of 4 straight cells and 4 corners: 16 treads a lap. The
    // first sector is a corner, so the walk opens on a landing at the base.
    let steps = walk(&ring, 8, CLICK_UNITS).unwrap();
    assert!(steps[0].landing && steps[0].rise == 0);
    let treads: Vec<_> = steps.iter().filter(|s| !s.landing).collect();
    assert_eq!(treads.len(), 8);
    assert_eq!(treads.last().unwrap().rise, 8 * CLICK_UNITS);
    // The corner after the top of the run is the arrival, flat at the top.
    assert!(steps.last().unwrap().landing);
    assert_eq!(steps.last().unwrap().rise, 8 * CLICK_UNITS);
    // Heights never fall and never jump by more than a riser.
    for pair in steps.windows(2) {
        let d = pair[1].rise - pair[0].rise;
        assert!(d == 0 || d == CLICK_UNITS);
    }
}

#[test]
fn a_ring_of_turns_only_has_nowhere_to_put_a_tread() {
    let ring = vec![[0, 0], [1, 0], [1, 1], [0, 1]];
    assert_eq!(
        walk(&ring, 4, CLICK_UNITS).unwrap_err(),
        SpiralRefusal::NoTreads
    );
}

#[test]
fn a_spiral_lays_a_tread_slab_per_visit_and_winds_through_a_sector_twice() {
    let mut world = hall(6);
    let ring = ring_cells(&square(6)).unwrap();
    // Two storeys of 16 clicks, a click a tread: 32 treads, two laps.
    let report = lay_spiral(
        &mut world,
        0,
        &ring,
        SpiralParams {
            storeys: 2,
            ..SpiralParams::default()
        },
        16 * CLICK_UNITS,
    );
    assert_eq!(report.refused, None, "{report:?}");
    assert_eq!((report.treads, report.riser), (32, CLICK_UNITS));
    assert_eq!(report.total, 32 * CLICK_UNITS);
    assert_eq!(report.laps, 3, "two laps and the arrival landing");
    assert_eq!(report.slabs.skipped.total(), 0, "{report:?}");
    // The first straight cell of the ring takes the first tread, one click
    // up (a raised floor: nothing under it is tall enough to stand in), and
    // the second lap's tread over the same sector is 16 treads higher.
    assert_eq!(floors(&world, [1, 0]), vec![1, 17]);
    assert_eq!(floors(&world, [2, 0]), vec![2, 18]);
    // A cell inside the ring is untouched.
    assert_eq!(floors(&world, [2, 2]), vec![0]);
    // The stairs are what the editor calls storeys' business: no extra
    // storey appears from a single stair's steps.
    assert!(storeys(&world).iter().all(|s| s.cells >= 8));
}

#[test]
fn a_tread_high_enough_leaves_a_walkable_undercroft_below_it() {
    let mut world = hall(6);
    let ring = ring_cells(&square(6)).unwrap();
    lay_spiral(
        &mut world,
        0,
        &ring,
        SpiralParams::default(),
        8 * CLICK_UNITS,
    );
    // Tread 5 sits 5 clicks up: the span under it keeps its floor and the
    // slab underside four clicks up, headroom enough to stand in.
    let room = &world.rooms[0];
    let [x, z] = room.local([5, 1]).unwrap();
    let column = room.sector(x, z).unwrap();
    let spans: Vec<(Option<i32>, Option<i32>)> = column
        .spans()
        .iter()
        .map(|s| {
            (
                s.floor.as_ref().map(|f| f.heights[0] / CLICK_UNITS),
                s.ceiling.as_ref().map(|c| c.heights[0] / CLICK_UNITS),
            )
        })
        .collect();
    // [5, 1] is the second straight cell down the east side: tread 6 + ...
    assert_eq!(spans.len(), 2, "{spans:?}");
    assert_eq!(spans[0].0, Some(0));
    assert!(spans[1].0.unwrap() >= 4);
}

#[test]
fn applying_nothing_puts_the_document_back_byte_for_byte() {
    let mut world = hall(6);
    let original = world.clone();
    let ring = ring_cells(&square(6)).unwrap();
    let drag = SpiralDrag::begin(&world, 0, &ring, SpiralParams::default()).unwrap();
    let laid = drag.apply(&mut world, 12 * CLICK_UNITS);
    assert!(laid.refused.is_none());
    assert_ne!(world, original);
    // A different climb replaces the first; zero takes it all back.
    drag.apply(&mut world, 6 * CLICK_UNITS);
    let none = drag.apply(&mut world, 0);
    assert_eq!(none.refused, Some(SpiralRefusal::NoClimb));
    assert_eq!(world, original);
}

#[test]
fn cells_that_are_no_ring_or_a_missing_room_refuse() {
    let mut world = hall(6);
    let original = world.clone();
    let line = vec![[0, 0], [1, 0], [2, 0], [3, 0]];
    let report = lay_spiral(
        &mut world,
        0,
        &line,
        SpiralParams::default(),
        CLICK_UNITS * 8,
    );
    assert_eq!(report.refused, Some(SpiralRefusal::NotARing));
    let report = lay_spiral(
        &mut world,
        4,
        &ring_cells(&square(6)).unwrap(),
        SpiralParams::default(),
        CLICK_UNITS * 8,
    );
    assert_eq!(report.refused, Some(SpiralRefusal::NoSuchRoom));
    assert_eq!(world, original);
}
