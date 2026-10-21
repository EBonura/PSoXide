//! Extrude moves hand-placed walls too: the picked pieces and the hand-placed
//! walls on the edges of the sectors the push makes or fills become derived
//! behaviour in the same step, carrying their look; nothing else is touched.

use super::extrude::{pieces_of, ExtrudeDrag, ExtrudeRefusal};
use super::tests::{hall, rid};
use crate::face_uv::FaceUv;
use crate::grid::derive::{edge_key, edge_walls};
use crate::grid::edit::bands::{edit_bands, BandScope};
use crate::grid::edit::{FaceRef, GridElement};
use crate::grid::faces::all_faces;
use crate::grid::overrides::WallMode;
use crate::grid::sector::WallEdge;
use crate::grid::surface::{GridWall, WallFacing};
use crate::grid::world::GridWorld;
use crate::grid::Anchor;
use crate::ResourceId;

const M: u64 = 7;

fn m() -> Option<ResourceId> {
    Some(rid(M))
}

/// A hand-placed wall record on the edge `edge` of world sector `cell`.
fn place(world: &mut GridWorld, cell: [i32; 2], edge: WallEdge, wall: GridWall) {
    let room = &mut world.rooms[0];
    let [x, z] = room.local(cell).expect("a sector of the room");
    room.sector_or_insert(x, z)
        .unwrap()
        .walls_mut(edge)
        .push(wall);
}

fn full() -> GridWall {
    GridWall::span(0, 3072, m())
}

fn stack(world: &GridWorld, cell: [i32; 2], edge: WallEdge) -> Vec<GridWall> {
    let room = &world.rooms[0];
    room.local(cell)
        .and_then(|[x, z]| room.sector(x, z))
        .map(|s| s.walls(edge).clone())
        .unwrap_or_default()
}

fn records(world: &GridWorld) -> usize {
    world.rooms[0].leaves().map(|(_, s)| s.wall_count()).sum()
}

/// A derived room with a ceiling whose back wall (row 0) is hand placed on
/// top of the derived closure the user hid, the way dp-world's hall is.
fn walled_hall() -> GridWorld {
    let mut world = hall(6, 6);
    for x in 0..6 {
        place(&mut world, [x, 0], WallEdge::North, full());
        world.rooms[0]
            .edge_overrides
            .edit(edge_key([x, 0], WallEdge::North), |o| o.hidden = true);
    }
    world
}

/// The same room with no ceiling and no closure wall height: only the
/// hand-placed walls close the north side, exactly dp-world's hall.
fn open_hall() -> GridWorld {
    let mut world = walled_hall();
    for z in 0..6 {
        for x in 0..6 {
            let [x, z] = [x as usize, z as usize];
            world.rooms[0]
                .sector_or_insert(x, z)
                .unwrap()
                .set_ground_ceiling(None);
        }
    }
    world
}

fn pick(cell: [i32; 2]) -> Vec<GridElement> {
    vec![GridElement::Face(FaceRef::wall(
        0,
        cell,
        WallEdge::North,
        0,
    ))]
}

fn extrude(
    world: &mut GridWorld,
    elements: &[GridElement],
    steps: i32,
) -> Result<(), ExtrudeRefusal> {
    let pieces = pieces_of(world, elements)?;
    ExtrudeDrag::begin(world, pieces)?.apply(world, steps)?;
    Ok(())
}

fn derived_material(world: &GridWorld, cell: [i32; 2], edge: WallEdge) -> Option<ResourceId> {
    let walls = edge_walls(&world.rooms[0], cell, edge);
    assert_eq!(walls.len(), 1, "one wall at {cell:?} {edge:?}");
    walls[0].material
}

fn floor_at(world: &GridWorld, cell: [i32; 2]) -> Option<[i32; 4]> {
    let room = &world.rooms[0];
    let [x, z] = room.local(cell)?;
    room.sector(x, z)?.ground_floor().map(|f| f.heights)
}

#[test]
fn a_hand_placed_wall_pulled_out_of_a_room_with_a_ceiling_makes_an_alcove_of_its_material() {
    let mut world = walled_hall();
    let others = records(&world) - 1;
    extrude(&mut world, &pick([2, 0]), 1).unwrap();
    assert_eq!(floor_at(&world, [2, -1]), Some([0; 4]));
    // The pulled record is gone and the other five are as they were.
    assert!(stack(&world, [2, 0], WallEdge::North).is_empty());
    assert_eq!(records(&world), others);
    for x in [0, 1, 3, 4, 5] {
        assert_eq!(stack(&world, [x, 0], WallEdge::North), vec![full()]);
    }
    // Nothing stands on the old edge; the alcove is closed by derived walls
    // that wear the pulled wall's material.
    assert!(edge_walls(&world.rooms[0], [2, 0], WallEdge::North).is_empty());
    for edge in [WallEdge::North, WallEdge::East, WallEdge::West] {
        assert_eq!(derived_material(&world, [2, -1], edge), m(), "{edge:?}");
    }
}

#[test]
fn a_hand_placed_wall_pulled_in_makes_a_column_with_derived_walls_of_its_material() {
    let mut world = walled_hall();
    extrude(&mut world, &pick([2, 0]), -1).unwrap();
    // The sector in front of the wall is solid floor to ceiling: gone.
    assert_eq!(floor_at(&world, [2, 0]), None);
    assert!(stack(&world, [2, 0], WallEdge::North).is_empty());
    for (cell, edge) in [
        ([1, 0], WallEdge::East),
        ([3, 0], WallEdge::West),
        ([2, 1], WallEdge::North),
    ] {
        assert_eq!(
            derived_material(&world, cell, edge),
            m(),
            "{cell:?} {edge:?}"
        );
    }
    // The wall beside the column was not picked and stays hand placed.
    assert_eq!(stack(&world, [1, 0], WallEdge::North), vec![full()]);
    assert_eq!(stack(&world, [3, 0], WallEdge::North), vec![full()]);
}

#[test]
fn in_a_hall_with_no_ceiling_pulling_in_raises_a_column_and_pulling_out_walls_a_corridor() {
    // In: the sector in front rises to the wall's top.
    let mut world = open_hall();
    extrude(&mut world, &pick([2, 0]), -1).unwrap();
    assert_eq!(floor_at(&world, [2, 0]), Some([3072; 4]));
    assert!(stack(&world, [2, 0], WallEdge::North).is_empty());
    for (cell, edge) in [
        ([1, 0], WallEdge::East),
        ([3, 0], WallEdge::West),
        ([2, 1], WallEdge::North),
    ] {
        assert_eq!(
            derived_material(&world, cell, edge),
            m(),
            "{cell:?} {edge:?}"
        );
    }
    // Out: a floor sector behind the wall, closed by hand-placed walls of
    // the same material where no derived wall can stand.
    let mut world = open_hall();
    extrude(&mut world, &pick([2, 0]), 1).unwrap();
    assert_eq!(floor_at(&world, [2, -1]), Some([0; 4]));
    assert!(stack(&world, [2, 0], WallEdge::North).is_empty());
    for edge in [WallEdge::North, WallEdge::East, WallEdge::West] {
        let walls = stack(&world, [2, -1], edge);
        assert_eq!(walls.len(), 1, "{edge:?}");
        assert_eq!(walls[0].material, m());
        assert_eq!(walls[0].heights, [0, 0, 3072, 3072]);
        assert!(walls[0].solid);
        assert_eq!(walls[0].facing, WallFacing::Front);
    }
    assert!(stack(&world, [2, -1], WallEdge::South).is_empty());
    // A second push carries on from the wall the first one made.
    let element = vec![GridElement::Face(FaceRef::wall(
        0,
        [2, -1],
        WallEdge::North,
        0,
    ))];
    extrude(&mut world, &element, 1).unwrap();
    assert_eq!(floor_at(&world, [2, -2]), Some([0; 4]));
    assert!(stack(&world, [2, -1], WallEdge::North).is_empty());
    assert_eq!(stack(&world, [2, -2], WallEdge::North).len(), 1);
}

#[test]
fn the_look_of_a_hand_placed_wall_on_a_touched_edge_moves_onto_the_derived_wall_there() {
    let mut world = walled_hall();
    // A divider between the sector that fills and its east neighbour, with
    // a look of its own: another material, a placement, a ghost.
    let uv = FaceUv {
        offset_texels: [5, 3],
        ..FaceUv::default()
    };
    let divider = GridWall {
        material: Some(rid(8)),
        uv,
        solid: false,
        band_set: Some(1),
        ..GridWall::span(0, 3072, None)
    };
    place(&mut world, [3, 0], WallEdge::West, divider);
    // A window-high piece of wall elsewhere on a touched edge matches no
    // derived face and stays what it is.
    let low = GridWall::span(0, 1024, Some(rid(9)));
    place(&mut world, [2, 1], WallEdge::West, low.clone());
    extrude(&mut world, &pick([2, 0]), -1).unwrap();
    assert!(stack(&world, [3, 0], WallEdge::West).is_empty());
    let over = world.rooms[0]
        .edge_overrides
        .get(&edge_key([3, 0], WallEdge::West))
        .cloned()
        .expect("the divider's look is on the derived wall now");
    assert_eq!(over.material, Some(rid(8)));
    assert_eq!(over.uv, Some(uv));
    assert_eq!(over.band_set, Some(1));
    assert_eq!(over.solid, Some(false));
    assert!(!over.hidden);
    assert_eq!(stack(&world, [2, 1], WallEdge::West), vec![low]);
}

#[test]
fn walls_the_push_does_not_touch_keep_their_records_and_overrides() {
    let mut world = walled_hall();
    let original = world.clone();
    extrude(&mut world, &pick([2, 0]), 1).unwrap();
    for x in [0, 4, 5] {
        assert_eq!(
            stack(&world, [x, 0], WallEdge::North),
            stack(&original, [x, 0], WallEdge::North)
        );
        let key = edge_key([x, 0], WallEdge::North);
        assert_eq!(
            world.rooms[0].edge_overrides.get(&key),
            original.rooms[0].edge_overrides.get(&key)
        );
    }
    // Every face of a sector away from the push is as it was.
    let far = |w: &GridWorld| {
        all_faces(w)
            .into_iter()
            .filter(|f| f.cell[0] >= 4)
            .map(|f| format!("{:?} {:?} {:?}", f.cell, f.points, f.material))
            .collect::<Vec<_>>()
    };
    assert_eq!(far(&world), far(&original));
}

#[test]
fn zero_steps_gives_the_document_back_and_a_push_repeats() {
    let world = open_hall();
    let pieces = pieces_of(&world, &pick([2, 0])).unwrap();
    let drag = ExtrudeDrag::begin(&world, pieces).unwrap();
    let mut live = world.clone();
    drag.apply(&mut live, 1).unwrap();
    let once = live.clone();
    drag.apply(&mut live, 2).unwrap();
    drag.apply(&mut live, 1).unwrap();
    assert_eq!(live, once);
    drag.apply(&mut live, -1).unwrap();
    drag.apply(&mut live, 0).unwrap();
    assert_eq!(live, world);
}

#[test]
fn a_room_of_hand_placed_walls_converts_in_the_same_step_and_looks_the_same_elsewhere() {
    // The classic room: every wall of its outline is a record.
    let mut world = hall(5, 5);
    world.rooms[0].wall_mode = WallMode::Authored;
    for i in 0..5 {
        place(&mut world, [i, 0], WallEdge::North, full());
        place(&mut world, [i, 4], WallEdge::South, full());
        place(&mut world, [0, i], WallEdge::West, full());
        place(&mut world, [4, i], WallEdge::East, full());
    }
    let look = |w: &GridWorld| {
        let mut v: Vec<String> = crate::grid::course_merge::merge_courses(all_faces(w))
            .into_iter()
            .filter(|f| f.cell[0] >= 3 || f.cell[1] >= 2)
            .filter(|f| f.cell != [3, 0] && f.cell != [3, -1] && f.cell != [2, 0])
            .map(|f| format!("{:?} {:?} {:?}", f.cell, f.points, f.material))
            .collect();
        v.sort();
        v
    };
    let original = world.clone();
    extrude(&mut world, &pick([1, 0]), 1).unwrap();
    assert_eq!(world.rooms[0].wall_mode, WallMode::Derived);
    assert_eq!(floor_at(&world, [1, -1]), Some([0; 4]));
    assert!(stack(&world, [1, 0], WallEdge::North).is_empty());
    assert_eq!(
        look(&world),
        look(&original),
        "the rest of the room reads the same"
    );
    // The walls it did not touch are derived faces of the room now, with
    // the material they had, and extrude like any derived wall.
    let closure = FaceRef::derived(
        0,
        [3, 0],
        WallEdge::North,
        crate::grid::derive::DerivedKind::Closure,
    );
    assert_eq!(closure.derived_wall(&world).and_then(|w| w.material), m());
    let pieces = pieces_of(&world, &[GridElement::Face(closure)]).unwrap();
    assert_eq!(pieces.len(), 1);
}

#[test]
fn a_banded_hand_placed_wall_pushes_one_band_out_as_a_niche() {
    let mut world = walled_hall();
    let face = FaceRef::wall(0, [2, 0], WallEdge::North, 0);
    assert!(edit_bands(&mut world, face, BandScope::Run, |set| {
        set.insert_line(0, Anchor::Abs(768)) && set.insert_line(1, Anchor::Abs(1536))
    }));
    let band = vec![GridElement::Band { face, band: 1 }];
    extrude(&mut world, &band, 1).unwrap();
    let floor = floor_at(&world, [2, -1]).expect("the niche has a floor");
    assert_eq!(floor, [768; 4]);
    assert!(stack(&world, [2, 0], WallEdge::North).is_empty());
}

#[test]
fn only_real_impossibilities_are_refused() {
    // A diagonal hand-placed wall has no sector to push into.
    let mut world = hall(4, 4);
    place(&mut world, [1, 1], WallEdge::Nwse, full());
    let diagonal = vec![GridElement::Face(FaceRef::wall(
        0,
        [1, 1],
        WallEdge::Nwse,
        0,
    ))];
    assert_eq!(pieces_of(&world, &diagonal), Err(ExtrudeRefusal::Diagonal));
    // A wall facing nothing (the room's own outside) has no floor to stand on.
    let mut world = hall(4, 4);
    place(
        &mut world,
        [1, 0],
        WallEdge::North,
        GridWall {
            facing: WallFacing::Back,
            ..full()
        },
    );
    let outward = pick([1, 0]);
    let refusal = pieces_of(&world, &outward)
        .and_then(|p| ExtrudeDrag::begin(&world, p))
        .unwrap_err();
    assert_eq!(refusal, ExtrudeRefusal::NoFloor);
    // Another room in the way stops it and leaves the document alone.
    let mut world = walled_hall();
    let mut other = crate::grid::room::GridRoom::new_derived("other", 1, 1);
    other.origin = [2, -1];
    other
        .sector_or_insert(0, 0)
        .unwrap()
        .set_ground_floor(Some(crate::grid::surface::GridSurface::flat_auto(0, None)));
    world.rooms.push(other);
    let before = world.clone();
    let err = extrude(&mut world, &pick([2, 0]), 1).unwrap_err();
    assert!(
        matches!(err, ExtrudeRefusal::OtherRoom { room: 1, .. }),
        "{err:?}"
    );
    assert_eq!(world, before);
}

#[test]
fn a_wall_seen_from_its_back_is_pushed_from_the_sector_it_is_seen_from() {
    let mut world = hall(4, 4);
    // A divider stored on the south sector, drawn towards the north one.
    let wall = GridWall {
        heights: [0, 512, 3072, 2560],
        facing: WallFacing::Back,
        ..full()
    };
    place(&mut world, [1, 2], WallEdge::North, wall);
    let element = vec![GridElement::Face(FaceRef::wall(
        0,
        [1, 2],
        WallEdge::North,
        0,
    ))];
    let pieces = pieces_of(&world, &element).unwrap();
    assert_eq!(pieces[0].host, [1, 1]);
    assert_eq!(pieces[0].edge, WallEdge::South);
    // Along the north sector's own walking direction the ends swap.
    assert_eq!(pieces[0].heights, [512, 0, 2560, 3072]);
}
