//! The Ramp and Stairs verbs against the real character motor: a ramp at the
//! steepest walkable slope is climbed, a steeper one is not, and a flight of
//! steps is climbed tread by tread.

use psx_engine::RoomPoint;

use super::motor_harness::{native, walk_path};
use crate::grid::edit::verbs::ramp::{ramp_region, RampDir, RampDrag, RampParams};
use crate::grid::edit::{CellRect, Region};
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A derived hall `w` sectors long and 3 wide, flat at 0, ceiling at 6144
/// (plenty of headroom for a ramp), at the default scale: 80 runtime units a
/// sector.
pub(super) fn hall(w: usize) -> GridWorld {
    let mut room = GridRoom::new_derived("hall", w, 3);
    for z in 0..3 {
        for x in 0..w {
            let sector = room.sector_or_insert(x, z).unwrap();
            sector.floor = Some(GridSurface::flat_auto(0, Some(rid(1))));
            sector.ceiling = Some(GridSurface::flat_auto(6144, Some(rid(2))));
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn params() -> RampParams {
    RampParams {
        from: RampDir::West,
        follow_ceiling: true,
        wall_material: None,
    }
}

/// Floors rise towards +x over sectors 3 to 8 of a 12 sector hall.
fn ramp_world(rise: i32) -> GridWorld {
    let mut world = hall(12);
    let region = Region::from_rect(0, CellRect::from_corners([3, 0], [8, 2]));
    ramp_region(&mut world, &region, params(), rise).unwrap();
    // The hall beyond the ramp stands at the ramp's top, flat.
    for x in 9..12 {
        for z in 0..3 {
            world.rooms[0]
                .sector_mut(x, z)
                .unwrap()
                .floor
                .as_mut()
                .unwrap()
                .heights = [rise; 4];
            world.rooms[0]
                .sector_mut(x, z)
                .unwrap()
                .ceiling
                .as_mut()
                .unwrap()
                .heights = [6144 + rise; 4];
        }
    }
    world
}

#[test]
fn the_steepest_walkable_ramp_is_climbed_by_the_real_motor_and_a_steeper_one_is_not() {
    let region = Region::from_rect(0, CellRect::from_corners([3, 0], [8, 2]));
    let drag = RampDrag::begin(&hall(12), &region, params()).unwrap();
    let steepest = drag.max_walkable_rise();
    assert!(
        steepest >= 6 * 1024 - 64,
        "six sectors at 45 degrees: {steepest}"
    );
    let units = super::RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    let top = units.rt(steepest);
    // The motor climbs it: start on the flat before the ramp, walk +x.
    let world = native(&ramp_world(steepest));
    let path = walk_path(&world, RoomPoint::new(100, 0, 120), 4096, 0, 700);
    let end = path.last().unwrap();
    assert!(end.x > 9 * 80 + 10, "got over the ramp: {end:?}");
    assert!(
        (end.y - top).abs() <= 2,
        "stands on the top, {top}: {end:?}"
    );
    // One fine step more and a row of the ramp is too steep for the motor to
    // stand on (the cook does not flag it walkable): the body never gets to
    // the top.
    let too_steep = native(&ramp_world(steepest + 64));
    let path = walk_path(&too_steep, RoomPoint::new(100, 0, 120), 4096, 0, 700);
    let top_steep = units.rt(steepest + 64);
    let best = path.iter().map(|p| p.y).max().unwrap();
    assert!(
        best < top_steep - 10 && best < top / 2,
        "a slope past the limit is not climbed: reached {best} of {top_steep}"
    );
}

#[test]
fn a_ramp_in_the_other_direction_walks_down() {
    let world = native(&ramp_world(-1024));
    let path = walk_path(&world, RoomPoint::new(100, 0, 120), 4096, 0, 700);
    let end = path.last().unwrap();
    assert!(end.x > 9 * 80, "{end:?}");
    assert!(end.y <= -78, "walked down to the bottom: {end:?}");
}

/// A hall whose floor from sector 4 on stands `rise` grid units up, a plain
/// riser the step rule judges.
fn riser_world(rise: i32) -> GridWorld {
    let mut world = hall(10);
    for x in 4..10 {
        for z in 0..3 {
            let sector = world.rooms[0].sector_mut(x, z).unwrap();
            sector.floor.as_mut().unwrap().heights = [rise; 4];
            sector.ceiling.as_mut().unwrap().heights = [6144 + rise; 4];
        }
    }
    world
}

#[test]
fn the_walkability_analysis_and_the_motor_agree_on_a_step() {
    use crate::grid::walk::{analyse, EdgeKind};
    let units = super::RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    for (rise, climbable) in [(512, true), (576, false)] {
        let world = riser_world(rise);
        let a = analyse(&world, &[], &psx_engine::WalkLimits::DEFAULT).unwrap();
        let flagged = a.edges.iter().any(|e| e.kind == EdgeKind::StepTooTall);
        assert_eq!(flagged, !climbable, "{rise} gu is {} ru", units.rt(rise));
        let path = walk_path(&native(&world), RoomPoint::new(100, 0, 120), 4096, 0, 300);
        let end = path.last().unwrap();
        if climbable {
            assert_eq!(end.y, units.rt(rise), "{end:?}");
            assert!(end.x > 4 * 80 + 10, "{end:?}");
        } else {
            assert_eq!(end.y, 0, "{end:?}");
            assert!(end.x < 4 * 80, "stopped at the riser: {end:?}");
        }
    }
}

#[test]
fn a_flight_of_stairs_is_climbed_tread_by_tread_by_the_real_motor() {
    use crate::grid::edit::verbs::stairs::{lay_stairs, RiserOption, StairsParams};
    let units = super::RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    // One click risers (20 runtime units) and the tallest the motor steps
    // up (40): both are climbed to the top.
    for (riser, rise) in [(RiserOption::Click, 6 * 256), (RiserOption::Max, 6 * 512)] {
        let mut world = hall(14);
        let params = StairsParams {
            riser,
            width: 3,
            ..StairsParams::default()
        };
        let flight = lay_stairs(&mut world, 0, &[[3, 1], [8, 1]], params, rise).unwrap();
        assert!(flight.refusal.is_none(), "{flight:?}");
        // The landing beyond the flight stands at its top.
        let top = units.rt(flight.total);
        for x in 9..14 {
            for z in 0..3 {
                let s = world.rooms[0].sector_mut(x, z).unwrap();
                s.floor.as_mut().unwrap().heights = [flight.total; 4];
                s.ceiling.as_mut().unwrap().heights = [6144 + flight.total; 4];
            }
        }
        let path = walk_path(&native(&world), RoomPoint::new(120, 0, 120), 4096, 0, 600);
        let end = path.last().unwrap();
        assert!(end.x > 9 * 80, "past the flight: {end:?} ({flight:?})");
        assert_eq!(end.y, top, "stands on the landing, {top}: {end:?}");
    }
}
