//! Tests of Move and Tilt.

use std::collections::BTreeSet;

use super::move_cells::{move_cells, MoveParams, MoveRefusal};
use super::tilt::{tilt_cells, BoxCorner, Hinge, TiltParams, TiltRefusal, TiltTarget};
use super::verbs::ramp::RampDir;
use super::verbs::room::{lay_room, Footprint, RoomParams};
use super::verbs::room_tests::empty;
use super::CellRect;
use crate::grid::leaf::LeafKey;
use crate::grid::mesh::GridMesh;
use crate::grid::world::GridWorld;
use crate::{NodeKind, Scene};

fn cells(a: [i32; 2], b: [i32; 2]) -> BTreeSet<[i32; 2]> {
    CellRect::from_corners(a, b).cells().collect()
}

fn room(w: &mut GridWorld, a: [i32; 2], b: [i32; 2]) -> usize {
    let mut p = RoomParams::new(Footprint::Rect(CellRect::from_corners(a, b)), "r", 3072);
    p.leaves = super::verbs::room::LeafPolicy::Sectors;
    lay_room(w, &p).unwrap().room
}

fn floor(w: &GridWorld, room: usize, cell: [i32; 2]) -> [i32; 4] {
    w.rooms[room]
        .leaf(LeafKey::sector(cell))
        .unwrap()
        .ground_floor()
        .unwrap()
        .heights
}

#[test]
fn move_carries_cells_nodes_and_meshes_and_refuses_a_collision() {
    let mut w = empty();
    let r = room(&mut w, [0, 0], [5, 3]);
    let mut scene = Scene::new("s");
    let sector = w.scale.sector_editor_units() as f32;
    let lamp = scene.add_node(
        scene.root,
        "lamp",
        NodeKind::PointLight {
            color: [255; 3],
            intensity: 1.0,
            radius: 4.0,
        },
    );
    scene.node_mut(lamp).unwrap().transform.translation = [sector * 1.5, 640.0, sector * 1.5];
    let far = scene.add_node(scene.root, "far", NodeKind::Entity);
    scene.node_mut(far).unwrap().transform.translation = [sector * 4.5, 0.0, sector * 1.5];
    w.rooms[r].meshes.push(GridMesh {
        id: 1,
        asset: 0,
        position: [1024 + 512, 0, 1024 + 512],
        rotation: [0; 3],
        scale: [256; 3],
        materials: Vec::new(),
        fullbright: false,
        collide: true,
        enabled: true,
    });
    let p = MoveParams {
        room: r,
        cells: cells([1, 1], [2, 2]),
        delta: [0, 0],
        dy_steps: 0,
        overwrite: false,
    };
    assert_eq!(move_cells(&mut w, None, &p), Err(MoveRefusal::NoChange));
    // Straight onto the cells that stay: refused, nothing changed.
    let before = w.clone();
    let hit = MoveParams {
        delta: [2, 0],
        ..p.clone()
    };
    assert!(matches!(
        move_cells(&mut w, Some(&mut scene), &hit),
        Err(MoveRefusal::Collision(_))
    ));
    assert_eq!(w, before);
    // Up two steps and out of the room's rectangle: the room grows.
    let up = MoveParams {
        delta: [0, 4],
        dy_steps: 2,
        overwrite: false,
        ..p.clone()
    };
    let out = move_cells(&mut w, Some(&mut scene), &up).unwrap();
    assert_eq!((out.leaves, out.meshes, out.nodes), (4, 1, 1));
    assert_eq!(floor(&w, r, [1, 5]), [128; 4]);
    assert!(w.rooms[r].leaf(LeafKey::sector([1, 1])).is_none());
    assert_eq!(w.rooms[r].meshes[0].position, [1536, 128, 1536 + 4096]);
    let moved = scene.node(lamp).unwrap().transform.translation;
    assert_eq!(moved[2], sector * 5.5);
    assert_eq!(
        scene.node(far).unwrap().transform.translation[2],
        sector * 1.5
    );
    // Overwrite replaces what is in the way.
    let over = MoveParams {
        cells: cells([1, 5], [2, 6]),
        delta: [0, -3],
        dy_steps: 0,
        overwrite: true,
        ..p
    };
    let out = move_cells(&mut w, None, &over).unwrap();
    assert_eq!(out.replaced, 2);
}

#[test]
fn a_leaf_cut_by_the_selection_stops_the_move() {
    let mut w = empty();
    let r = room(&mut w, [0, 0], [3, 3]);
    let sub = cells([0, 0], [1, 1]);
    let p = MoveParams {
        room: r,
        cells: sub,
        delta: [8, 0],
        dy_steps: 0,
        overwrite: false,
    };
    let mut big = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([12, 0], [15, 3])),
        "big",
        3072,
    );
    big.leaves = super::verbs::room::LeafPolicy::Maximal;
    // A coarse leaf room: the default policy makes one big leaf.
    lay_room(&mut w, &big).unwrap();
    let q = MoveParams {
        cells: cells([12, 0], [13, 1]),
        room: 1,
        ..p.clone()
    };
    assert!(matches!(
        move_cells(&mut w, None, &q),
        Err(MoveRefusal::LeafCrossesEdge(_))
    ));
    assert!(move_cells(&mut w, None, &p).is_ok());
}

#[test]
fn tilt_leans_a_platform_without_flattening_it() {
    let mut w = empty();
    let r = room(&mut w, [0, 0], [3, 0]);
    // A platform on the third cell, a pit on the first.
    w.rooms[r]
        .leaf_mut(LeafKey::sector([2, 0]))
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .heights = [256; 4];
    w.rooms[r]
        .leaf_mut(LeafKey::sector([0, 0]))
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .heights = [-256; 4];
    let p = TiltParams {
        room: r,
        cells: cells([0, 0], [3, 0]),
        target: TiltTarget::Floor,
        hinge: Hinge::Edge(RampDir::West),
        rise: 512,
        force: false,
    };
    let out = tilt_cells(&mut w, &p).unwrap();
    assert_eq!(out.cells.len(), 4);
    // A 512 rise over 4 sectors: 128 per sector. West corners 0, 128, ...
    assert_eq!(floor(&w, r, [0, 0]), [-256, -128, -128, -256]);
    assert_eq!(floor(&w, r, [1, 0]), [128, 256, 256, 128]);
    assert_eq!(floor(&w, r, [2, 0]), [512, 640, 640, 512]);
    // Rounding is reported per cell and is a multiple of the lattice.
    assert!(out
        .cells
        .iter()
        .all(|c| c.applied.iter().all(|a| a % 64 == 0)));
    assert_eq!(out.cells[1].exact_16ths, [2048, 4096, 4096, 2048]);
}

#[test]
fn tilt_refuses_a_too_steep_floor_unless_forced_and_never_through_the_ceiling() {
    let mut w = empty();
    let r = room(&mut w, [0, 0], [1, 1]);
    let steep = TiltParams {
        room: r,
        cells: cells([0, 0], [1, 1]),
        target: TiltTarget::Both,
        hinge: Hinge::Corner(BoxCorner::NorthWest),
        rise: 4096,
        force: false,
    };
    let before = w.clone();
    assert!(matches!(
        tilt_cells(&mut w, &steep),
        Err(TiltRefusal::TooSteep(_))
    ));
    assert_eq!(w, before);
    let forced = TiltParams {
        force: true,
        ..steep.clone()
    };
    assert!(tilt_cells(&mut w, &forced).is_ok());
    // Through the ceiling.
    let mut w = empty();
    let r = room(&mut w, [0, 0], [3, 3]);
    let up = TiltParams {
        room: r,
        cells: cells([0, 0], [3, 3]),
        target: TiltTarget::Floor,
        hinge: Hinge::Edge(RampDir::North),
        rise: 3072,
        force: true,
    };
    assert!(matches!(
        tilt_cells(&mut w, &up),
        Err(TiltRefusal::MeetsCeiling(_))
    ));
}
