//! Extrude on walls and ceilings: a wall piece pushed out makes an alcove,
//! a band a niche, pushed in it fills; ceilings move up and down. The
//! document comes back exactly at zero steps.

use super::extrude::{pieces_of, CeilingDrag, ExtrudeDrag, ExtrudeRefusal};
use super::tests::{ceiling, floor, hall, rid};
use crate::grid::derive::{edge_walls, DerivedKind};
use crate::grid::edit::bands::{edit_bands, BandScope};
use crate::grid::edit::region::Region;
use crate::grid::edit::{CellRect, FaceRef, GridElement};
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::surface::GridSurface;
use crate::grid::units::CLICK_UNITS;
use crate::grid::world::GridWorld;
use crate::grid::Anchor;

fn wall(world: &GridWorld, cell: [i32; 2], edge: WallEdge) -> GridElement {
    // The wall of a hall edge is its closure.
    let face = FaceRef::derived(0, cell, edge, DerivedKind::Closure);
    assert!(face.derived_wall(world).is_some(), "no wall at {cell:?}");
    GridElement::Face(face)
}

fn extrude(
    world: &mut GridWorld,
    elements: &[GridElement],
    steps: i32,
) -> Result<ExtrudeDrag, ExtrudeRefusal> {
    let pieces = pieces_of(world, elements)?;
    let drag = ExtrudeDrag::begin(world, pieces)?;
    drag.apply(world, steps)?;
    Ok(drag)
}

fn sector_floor(world: &GridWorld, cell: [i32; 2]) -> Option<[i32; 4]> {
    let room = &world.rooms[0];
    let [x, z] = room.local(cell)?;
    room.sector(x, z)?.ground_floor().map(|f| f.heights)
}

fn sector_ceiling(world: &GridWorld, cell: [i32; 2]) -> Option<[i32; 4]> {
    let room = &world.rooms[0];
    let [x, z] = room.local(cell)?;
    room.sector(x, z)?.ground_ceiling().map(|f| f.heights)
}

#[test]
fn a_closure_wall_pushed_out_makes_an_alcove_closed_by_derived_walls() {
    let mut world = hall(6, 6);
    let north = wall(&world, [2, 0], WallEdge::North);
    extrude(&mut world, &[north], 1).unwrap();
    assert_eq!(sector_floor(&world, [2, -1]), Some([0; 4]));
    assert_eq!(sector_ceiling(&world, [2, -1]), Some([3072; 4]));
    let room = &world.rooms[0];
    // The wall that stood on the old edge is gone; the alcove has three.
    assert!(edge_walls(room, [2, 0], WallEdge::North).is_empty());
    for edge in [WallEdge::North, WallEdge::East, WallEdge::West] {
        let walls = edge_walls(room, [2, -1], edge);
        assert_eq!(walls.len(), 1, "{edge:?}");
        assert_eq!(walls[0].kind, DerivedKind::Closure);
    }
    // The floor and ceiling take the host's materials.
    let new = room.sector(2, 0).unwrap();
    assert_eq!(new.ground_floor().unwrap().tri_a.material, Some(rid(1)));
    assert_eq!(new.ground_ceiling().unwrap().tri_a.material, Some(rid(2)));
}

#[test]
fn two_steps_make_a_tunnel_and_zero_steps_give_the_document_back() {
    let mut world = hall(6, 6);
    let original = world.clone();
    let north = wall(&world, [2, 0], WallEdge::North);
    let pieces = pieces_of(&world, &[north]).unwrap();
    let drag = ExtrudeDrag::begin(&world, pieces).unwrap();
    drag.apply(&mut world, 2).unwrap();
    assert!(sector_floor(&world, [2, -1]).is_some() && sector_floor(&world, [2, -2]).is_some());
    drag.apply(&mut world, 1).unwrap();
    assert!(
        sector_floor(&world, [2, -2]).is_none(),
        "one step is shorter"
    );
    drag.apply(&mut world, 0).unwrap();
    assert_eq!(world, original);
    // Dragging through the other side and back is exact too.
    drag.apply(&mut world, -1).unwrap();
    drag.apply(&mut world, 3).unwrap();
    drag.apply(&mut world, 0).unwrap();
    assert_eq!(world, original);
}

#[test]
fn a_band_pushed_out_makes_a_niche_at_the_band_with_a_step_up_and_a_drop() {
    let mut world = hall(6, 6);
    let north = FaceRef::derived(0, [2, 0], WallEdge::North, DerivedKind::Closure);
    assert!(edit_bands(&mut world, north, BandScope::Run, |set| {
        set.insert_line(0, Anchor::Abs(768)) && set.insert_line(1, Anchor::Abs(1536))
    }));
    // Three bands now: 0..768, 768..1536, 1536..3072. Take the middle one.
    extrude(
        &mut world,
        &[GridElement::Band {
            face: north,
            band: 1,
        }],
        1,
    )
    .unwrap();
    assert_eq!(sector_floor(&world, [2, -1]), Some([768; 4]));
    assert_eq!(sector_ceiling(&world, [2, -1]), Some([1536; 4]));
    let room = &world.rooms[0];
    let seen = edge_walls(room, [2, 0], WallEdge::North);
    let kinds: Vec<DerivedKind> = seen.iter().map(|w| w.kind).collect();
    assert!(kinds.contains(&DerivedKind::Riser), "{kinds:?}");
    assert!(kinds.contains(&DerivedKind::Drop), "{kinds:?}");
    // The niche is closed on its other three sides.
    for edge in [WallEdge::North, WallEdge::East, WallEdge::West] {
        assert_eq!(edge_walls(room, [2, -1], edge).len(), 1, "{edge:?}");
    }
}

#[test]
fn pushing_a_wall_in_fills_the_cell_in_front_of_it_solid() {
    let mut world = hall(6, 6);
    let north = wall(&world, [2, 0], WallEdge::North);
    extrude(&mut world, &[north], -1).unwrap();
    let room = &world.rooms[0];
    assert!(room.sector(2, 0).and_then(|s| s.ground_floor()).is_none());
    // The wall stands one sector in now, on the neighbour's north edge.
    assert_eq!(edge_walls(room, [2, 1], WallEdge::North).len(), 1);
}

#[test]
fn a_riser_pushed_in_widens_the_platform_and_pushed_out_cuts_it_back() {
    let mut world = hall(6, 6);
    // A platform at (2,1), 256 up; its riser seen from (1,1).
    {
        let room = &mut world.rooms[0];
        let s = room.sector_or_insert(2, 1).unwrap();
        s.ground_floor_mut().unwrap().heights = [256; 4];
    }
    let riser = GridElement::Face(FaceRef::derived(
        0,
        [1, 1],
        WallEdge::East,
        DerivedKind::Riser,
    ));
    let original = world.clone();
    extrude(&mut world, &[riser], -1).unwrap();
    assert_eq!(
        sector_floor(&world, [1, 1]),
        Some([256; 4]),
        "platform grew into (1,1)"
    );
    assert_eq!(sector_floor(&world, [0, 1]), Some([0; 4]));
    let mut world = original;
    extrude(&mut world, &[riser], 1).unwrap();
    assert_eq!(
        sector_floor(&world, [2, 1]),
        Some([0; 4]),
        "platform cut back"
    );
}

#[test]
fn a_band_floating_between_floor_and_ceiling_cannot_be_pushed_in() {
    let mut world = hall(6, 6);
    let north = FaceRef::derived(0, [2, 0], WallEdge::North, DerivedKind::Closure);
    assert!(edit_bands(&mut world, north, BandScope::Run, |set| {
        set.insert_line(0, Anchor::Abs(768)) && set.insert_line(1, Anchor::Abs(1536))
    }));
    let original = world.clone();
    let result = extrude(
        &mut world,
        &[GridElement::Band {
            face: north,
            band: 1,
        }],
        -1,
    );
    assert_eq!(result.unwrap_err(), ExtrudeRefusal::Floating);
    assert_eq!(world, original, "a refusal changes nothing");
}

#[test]
fn another_room_behind_the_wall_stops_the_extrusion_without_changing_anything() {
    let mut world = hall(6, 6);
    let mut annex = GridRoom::new_derived("annex", 2, 2);
    annex.origin = [2, -2];
    for z in 0..2 {
        for x in 0..2 {
            let s = annex.sector_or_insert(x, z).unwrap();
            s.set_ground_floor(Some(GridSurface::flat_auto(0, None)));
            s.set_ground_ceiling(Some(GridSurface::flat_auto(3072, None)));
        }
    }
    world.rooms.push(annex);
    let original = world.clone();
    let north = wall(&world, [2, 0], WallEdge::North);
    let err = extrude(&mut world, &[north], 1).unwrap_err();
    assert!(
        matches!(err, ExtrudeRefusal::OtherRoom { room: 1, .. }),
        "{err:?}"
    );
    assert_eq!(world, original);
}

#[test]
fn a_run_of_pieces_goes_out_together_and_mixed_directions_are_refused() {
    let mut world = hall(6, 6);
    let run: Vec<GridElement> = (1..=3)
        .map(|x| wall(&world, [x, 0], WallEdge::North))
        .collect();
    extrude(&mut world, &run, 1).unwrap();
    for x in 1..=3 {
        assert!(sector_floor(&world, [x, -1]).is_some(), "{x}");
    }
    // One north and one south wall are not coplanar.
    let world = hall(6, 6);
    let mixed = [
        wall(&world, [1, 0], WallEdge::North),
        wall(&world, [1, 5], WallEdge::South),
    ];
    let pieces = pieces_of(&world, &mixed).unwrap();
    assert_eq!(
        ExtrudeDrag::begin(&world, pieces).unwrap_err(),
        ExtrudeRefusal::MixedFacing
    );
}

#[test]
fn rooms_with_hand_placed_walls_are_pointed_at_the_conversion() {
    let mut world = hall(4, 4);
    world.rooms[0].wall_mode = crate::grid::overrides::WallMode::Authored;
    let element = GridElement::Face(FaceRef::wall(0, [1, 0], WallEdge::North, 0));
    assert!(matches!(
        pieces_of(&world, &[element]),
        Err(ExtrudeRefusal::AuthoredWalls)
    ));
}

#[test]
fn ceilings_move_with_the_region_and_stay_a_click_above_the_floor() {
    let mut world = hall(6, 6);
    let region = Region::from_rect(0, CellRect::from_corners([2, 2], [3, 3]));
    let original = world.clone();
    let drag = CeilingDrag::begin(&world, &region).unwrap();
    let applied = drag.apply(&mut world, 2 * CLICK_UNITS);
    assert_eq!(applied.moved, 4);
    assert_eq!(ceiling(&world, [2, 2]).heights, [3584; 4]);
    assert_eq!(
        ceiling(&world, [1, 2]).heights,
        [3072; 4],
        "outside keeps its own"
    );
    assert_eq!(floor(&world, [2, 2]).heights, [0; 4]);
    // A derived drop hangs from the raised ceiling, seen from inside the block.
    assert!(edge_walls(&world.rooms[0], [2, 2], WallEdge::West)
        .iter()
        .any(|w| w.kind == DerivedKind::Drop));
    // Far down: held a click over the floor.
    let applied = drag.apply(&mut world, -100 * CLICK_UNITS);
    assert_eq!(ceiling(&world, [2, 2]).heights, [CLICK_UNITS; 4]);
    assert_eq!(applied.clamped, 4);
    drag.apply(&mut world, 0);
    assert_eq!(world, original);
}
