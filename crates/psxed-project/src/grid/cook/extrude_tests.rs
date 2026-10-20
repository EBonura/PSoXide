//! Every Extrude case cooks to a level that reads back and validates:
//! floor (Raise), ceiling, wall out (alcove), band out (niche), wall in.

use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{LevelView, RoomView};

use super::tests::cook;
use crate::grid::derive::DerivedKind;
use crate::grid::edit::bands::{edit_bands, BandScope};
use crate::grid::edit::region::Region;
use crate::grid::edit::verbs::extrude::{pieces_of, CeilingDrag, ExtrudeDrag};
use crate::grid::edit::verbs::raise::{RaiseDrag, RaiseMode, RaiseParams};
use crate::grid::edit::{CellRect, FaceRef, GridElement};
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::surface::GridSurface;
use crate::grid::units::{GridScale, CLICK_UNITS};
use crate::grid::world::GridWorld;
use crate::grid::Anchor;

fn hall() -> GridWorld {
    let mut room = GridRoom::new_derived("hall", 6, 6);
    for z in 0..6 {
        for x in 0..6 {
            let s = room.sector_or_insert(x, z).unwrap();
            s.set_ground_floor(Some(GridSurface::flat_auto(0, None)));
            s.set_ground_ceiling(Some(GridSurface::flat_auto(3072, None)));
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// Cook, read back and validate every chunk; returns the room's quad count.
fn cooks(world: &GridWorld) -> usize {
    let cooked = cook(world).expect("the level cooks");
    let core = aligned_copy(&cooked.core);
    let level = LevelView::parse(words_as_bytes(&core, cooked.core.len())).unwrap();
    level.validate_full().unwrap();
    let mut quads = 0;
    for chunk in &cooked.chunks {
        let words = aligned_copy(chunk);
        let room = RoomView::parse(words_as_bytes(&words, chunk.len())).unwrap();
        room.validate_full(level.materials.len()).unwrap();
        quads += room.render.quads.len();
    }
    quads
}

fn north_wall(cell: [i32; 2]) -> FaceRef {
    FaceRef::derived(0, cell, WallEdge::North, DerivedKind::Closure)
}

#[test]
fn extruding_a_floor_cooks() {
    let mut world = hall();
    let region = Region::from_rect(0, CellRect::from_corners([2, 2], [3, 3]));
    let params = RaiseParams {
        mode: RaiseMode::Offset,
        follow_ceiling: false,
        wall_material: None,
        reference: 0,
    };
    let drag = RaiseDrag::begin(&world, &region, params).unwrap();
    drag.apply(&mut world, 2 * CLICK_UNITS);
    cooks(&world);
}

#[test]
fn extruding_a_ceiling_cooks() {
    let mut world = hall();
    let region = Region::from_rect(0, CellRect::from_corners([2, 2], [3, 3]));
    let drag = CeilingDrag::begin(&world, &region).unwrap();
    drag.apply(&mut world, 2 * CLICK_UNITS);
    let raised = cooks(&world);
    drag.apply(&mut world, -4 * CLICK_UNITS);
    assert!(cooks(&world) > 0 && raised > 0);
}

#[test]
fn a_wall_pushed_out_cooks_and_walls_the_alcove_in() {
    let mut world = hall();
    let before = cooks(&world);
    let pieces = pieces_of(&world, &[GridElement::Face(north_wall([2, 0]))]).unwrap();
    ExtrudeDrag::begin(&world, pieces)
        .unwrap()
        .apply(&mut world, 2)
        .unwrap();
    // A floor and ceiling per new sector, three walls each, less the wall
    // that stood on the old edge and the end wall's neighbour sides.
    assert!(cooks(&world) > before);
    // The new sectors collide: the closure walls come out as wall records.
    let cooked = cook(&world).unwrap();
    assert!(cooked.tables[0].walls.len() > 24);
}

#[test]
fn a_band_pushed_out_makes_a_niche_that_cooks() {
    let mut world = hall();
    let face = north_wall([2, 0]);
    assert!(edit_bands(&mut world, face, BandScope::Run, |set| {
        set.insert_line(0, Anchor::Abs(768)) && set.insert_line(1, Anchor::Abs(1536))
    }));
    let pieces = pieces_of(&world, &[GridElement::Band { face, band: 1 }]).unwrap();
    ExtrudeDrag::begin(&world, pieces)
        .unwrap()
        .apply(&mut world, 1)
        .unwrap();
    cooks(&world);
}

#[test]
fn a_wall_pushed_in_cooks() {
    let mut world = hall();
    let pieces = pieces_of(&world, &[GridElement::Face(north_wall([2, 0]))]).unwrap();
    ExtrudeDrag::begin(&world, pieces)
        .unwrap()
        .apply(&mut world, -2)
        .unwrap();
    cooks(&world);
}
