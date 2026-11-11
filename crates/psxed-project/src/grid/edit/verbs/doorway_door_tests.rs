//! Tests of the doorway-with-door helper.

use crate::grid::edit::CellRect;
use crate::grid::sector::WallEdge;
use crate::{
    DoorDirection, DoorLock, InteractableKind, LogicNodeKind, NodeKind, ProjectDocument,
    SwitchBehavior,
};

use super::doorway_door::{doorway_with_door, DoorwayDoor, Lever};
use super::opening::{OpeningKind, OpeningParams, OpeningProfile};
use super::room::{lay_room, Footprint, LeafPolicy, RoomParams};
use super::room_tests::empty;

fn level() -> crate::grid::world::GridWorld {
    let mut w = empty();
    // Two rooms side by side, a wall between them at x = 4.
    for (x0, name) in [(0, "a"), (4, "b")] {
        let mut p = RoomParams::new(
            Footprint::Rect(CellRect::from_corners([x0, 0], [x0 + 3, 2])),
            name,
            3072,
        );
        p.leaves = LeafPolicy::Sectors;
        lay_room(&mut w, &p).unwrap();
    }
    w
}

fn params() -> DoorwayDoor {
    DoorwayDoor {
        opening: OpeningParams {
            profile: OpeningProfile::Rectangular,
            room: 0,
            cell: [3, 1],
            side: WallEdge::East,
            from: 4,
            to: 8,
            kind: OpeningKind::Door,
            top: 2048,
            material: None,
        },
        name: "gate".into(),
        material: None,
        direction: DoorDirection::Up,
        ticks: 60,
        lock: DoorLock::Unlocked,
        refusal: String::new(),
        persistent: true,
        lever: Some(Lever {
            cell: [1, 1],
            prompt: "PULL".into(),
            radius: 640,
            behavior: SwitchBehavior::OneShot,
        }),
    }
}

#[test]
fn one_call_cuts_the_doorway_and_places_the_door_slab_and_lever() {
    let mut w = level();
    let mut project = ProjectDocument::new("t");
    let scene = project.active_scene_mut();
    let out = doorway_with_door(&mut w, scene, &params()).unwrap();
    // The slab is sized to the plane: 1024 grid units wide, 2048 tall.
    let slab = scene.node(out.slab).unwrap();
    let NodeKind::BoxProp { vertices, .. } = &slab.kind else {
        panic!("a box prop")
    };
    let ed = |gu| w.scale.to_editor(gu) as i16;
    assert_eq!(
        vertices[2][2],
        ed(out.plane.width) / 2,
        "half width along z"
    );
    assert_eq!(vertices[4][1], ed(out.plane.height));
    assert_eq!(
        slab.transform.translation,
        [
            w.scale.to_editor(out.plane.center[0]) as f32,
            w.scale.to_editor(out.plane.center[1]) as f32,
            w.scale.to_editor(out.plane.center[2]) as f32
        ]
    );
    // The door node names the slab and slides it by its height.
    let door = scene.node(out.door).unwrap();
    let NodeKind::Logic {
        kind: LogicNodeKind::Door {
            box_prop, slide, ..
        },
        ..
    } = &door.kind
    else {
        panic!("a door")
    };
    assert_eq!(box_prop, "gate box");
    assert_eq!(
        slide.as_ref().unwrap().distance as i32,
        ed(out.plane.height) as i32
    );
    // The lever targets the door.
    let lever = scene.node(out.lever.unwrap()).unwrap();
    let child = scene.node(lever.children[0]).unwrap();
    let NodeKind::Interactable {
        kind: InteractableKind::Switch { target, .. },
        ..
    } = &child.kind
    else {
        panic!("a switch")
    };
    assert_eq!(target, "gate");
    // The grid is cut and mirrored.
    assert_eq!(out.plane.width, 1024);
    crate::grid::cook::tests::cook(&w).expect("cooks");
}

#[test]
fn a_refused_cut_places_nothing() {
    let mut w = level();
    let mut project = ProjectDocument::new("t");
    let scene = project.active_scene_mut();
    let before = scene.nodes().len();
    let mut p = params();
    p.opening.top = 100;
    assert!(doorway_with_door(&mut w, scene, &p).is_err());
    assert_eq!(scene.nodes().len(), before);
}
