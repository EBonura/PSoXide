//! The cooked surface is watertight: no open edge has nothing beside it.
//!
//! A hole in the cooked mesh is where Play shows the void, and the editor's
//! preview shows it as a dark slit. The check reads the cooked quads
//! ([`super::mesh::open_edges`]), so it judges what is drawn.

use super::mesh::open_edges;
use super::tests::cook;
use super::*;
use crate::grid::edit::natural::{set_void, Triangles};
use crate::grid::edit::refs::SurfaceKind;
use crate::grid::edit::region::Region;
use crate::grid::edit::verbs::break_verb::{break_region, BreakParams, BreakTargets};
use crate::grid::edit::CellRect;
use crate::grid::room::GridRoom;
use crate::grid::surface::{GridSplit, GridSurface};
use crate::grid::units::GridScale;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A derived 12 by 12 hall (floor 0, ceiling 2560) with an octagonal pillar
/// cut out of the middle the way the cave proof does it: five whole cells
/// voided, four corner cells with one triangle voided and the diagonal
/// chosen for it. `pin` says whether the corners' diagonals are pinned (an
/// explicit choice) or left to follow the heights.
fn pillar_hall(pin: bool) -> GridWorld {
    let mut room = GridRoom::new_derived("hall", 12, 12);
    for z in 0..12 {
        for x in 0..12 {
            let sector = room.sector_or_insert(x, z).unwrap();
            sector.floor = Some(GridSurface::flat_auto(0, Some(rid(1))));
            sector.ceiling = Some(GridSurface::flat_auto(2560, Some(rid(2))));
        }
    }
    let mut world = GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    };
    for cell in [[6, 6], [5, 6], [7, 6], [6, 5], [6, 7]] {
        for kind in [SurfaceKind::Floor, SurfaceKind::Ceiling] {
            set_void(&mut world, 0, cell, kind, Triangles::Both, true);
        }
    }
    for (cell, split, tri) in [
        ([5, 5], GridSplit::NeSw, Triangles::B),
        ([7, 5], GridSplit::NwSe, Triangles::B),
        ([5, 7], GridSplit::NwSe, Triangles::A),
        ([7, 7], GridSplit::NeSw, Triangles::A),
    ] {
        for kind in [SurfaceKind::Floor, SurfaceKind::Ceiling] {
            let [x, z] = world.rooms[0].local(cell).unwrap();
            let sector = world.rooms[0].sector_mut(x, z).unwrap();
            let surface = match kind {
                SurfaceKind::Floor => sector.floor.as_mut(),
                SurfaceKind::Ceiling => sector.ceiling.as_mut(),
            }
            .unwrap();
            surface.split = split;
            surface.auto_split = !pin;
            set_void(&mut world, 0, cell, kind, tri, true);
        }
    }
    world
}

fn holes(world: &GridWorld) -> Vec<String> {
    let cooked = cook(world).expect("the world cooks");
    open_edges(&cooked.tables[0])
        .iter()
        .filter(|e| e.hole)
        .map(|e| format!("{:?} -> {:?} (quad {})", e.a, e.b, e.quad))
        .collect()
}

fn every_target() -> BreakParams {
    BreakParams {
        targets: BreakTargets {
            walls: true,
            floor: true,
            ceiling: true,
        },
        keep_walkable: false,
        ..BreakParams::default()
    }
}

#[test]
fn a_pillar_is_watertight_as_authored() {
    for pin in [true, false] {
        let found = holes(&pillar_hall(pin));
        assert!(found.is_empty(), "pinned {pin}: {found:?}");
    }
}

#[test]
fn the_check_finds_a_floor_and_ceiling_that_disagree_about_the_void() {
    // Flip the floor's diagonal under a voided corner but not the ceiling's:
    // the void half is a different triangle above and below, so the closure
    // wall meets one of them and the other ends in the open.
    let mut world = pillar_hall(true);
    let [x, z] = world.rooms[0].local([7, 5]).unwrap();
    let floor = world.rooms[0]
        .sector_mut(x, z)
        .unwrap()
        .floor
        .as_mut()
        .unwrap();
    floor.split = GridSplit::NeSw;
    let found = holes(&world);
    assert!(!found.is_empty(), "the flipped diagonal must open a hole");
}

#[test]
fn breaking_the_rock_around_a_pillar_never_opens_a_hole() {
    // The bug this pins: `Break` tilts interior vertices and lets a surface
    // that follows its heights choose a new diagonal, which moved the void
    // triangle of a pillar corner to the other half of the cell. Diagonals
    // next to a void stay as authored, pinned or not.
    let region = Region::from_rect(0, CellRect::from_corners([1, 1], [10, 10]));
    for pin in [true, false] {
        for seed in 1..=12 {
            let mut world = pillar_hall(pin);
            break_region(&mut world, &region, every_target(), 100, seed).unwrap();
            let found = holes(&world);
            assert!(found.is_empty(), "pinned {pin} seed {seed}: {found:?}");
        }
    }
}
