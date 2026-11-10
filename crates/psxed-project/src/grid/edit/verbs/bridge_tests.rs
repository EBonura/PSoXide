//! Tests of the Bridge verb.

use crate::grid::edit::CellRect;
use crate::grid::mesh::{MeshKind, Proxy};

use super::bridge::{build_bridge, BridgeParams, BridgeRefusal};
use super::room::{lay_room, Footprint, LeafPolicy, RoomParams, RoomTarget};
use super::room_tests::empty;

fn world() -> crate::grid::world::GridWorld {
    let mut w = empty();
    let mut p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [11, 3])),
        "gap",
        8192,
    );
    p.leaves = LeafPolicy::Sectors;
    lay_room(&mut w, &p).unwrap();
    w
}

fn deck() -> BridgeParams {
    BridgeParams::deck(
        RoomTarget::Existing(0),
        [2048, 0, 2048],
        [10240, 0, 2048],
        1024,
        512,
    )
}

#[test]
fn a_deck_is_one_instance_with_one_walkable_slab() {
    let mut w = world();
    let out = build_bridge(&mut w, &deck()).unwrap();
    assert_eq!((out.length, out.rise), (8192, 0));
    assert!(out.parapets.is_empty() && out.piers.is_empty());
    let room = &w.rooms[0];
    assert_eq!(room.meshes.len(), 1);
    let m = &room.meshes[0];
    assert_eq!(m.position, [6144, 0, 2048]);
    let MeshKind::Span(spec) = &w.mesh_assets[usize::from(m.asset)].kind else {
        panic!("a span")
    };
    assert_eq!((spec.length, spec.width, spec.thickness), (8192, 1024, 512));
    let built = w.mesh_assets[usize::from(m.asset)].build().unwrap();
    assert_eq!(
        built
            .proxies
            .iter()
            .filter(|p| matches!(p, Proxy::Slab { .. }))
            .count(),
        1
    );
}

#[test]
fn parapets_and_piers_come_with_their_rules() {
    let mut w = world();
    let mut p = deck();
    p.parapet = Some(768);
    p.pier_spacing = Some(2048);
    p.pier_foot = -4096;
    let out = build_bridge(&mut w, &p).unwrap();
    assert_eq!(out.parapets.len(), 2);
    assert_eq!(out.piers.len(), 3, "piers at 2048, 4096, 6144 of 8192");
    // Parapets stand on the two edges and top the deck by their height.
    let room = &w.rooms[0];
    let zs: Vec<i32> = room
        .meshes
        .iter()
        .skip(1)
        .take(2)
        .map(|m| m.position[2])
        .collect();
    assert_eq!(
        zs.iter().map(|z| (z - 2048).abs()).collect::<Vec<_>>(),
        vec![448, 448]
    );
    assert_eq!(room.meshes[1].position[1], 768);
    // A parapet the body can step over is refused.
    let mut low = deck();
    low.parapet = Some(256);
    assert!(matches!(
        build_bridge(&mut world(), &low),
        Err(BridgeRefusal::ParapetTooLow { .. })
    ));
}

#[test]
fn a_climbing_deck_is_held_to_the_walkable_slope() {
    let mut w = world();
    let mut p = deck();
    p.to[1] = 4096;
    build_bridge(&mut w, &p).unwrap();
    p.to[1] = 8000;
    let before = w.clone();
    assert!(matches!(
        build_bridge(&mut w, &p),
        Err(BridgeRefusal::TooSteep { max_rise: 7782 })
    ));
    assert!(w == before);
    let mut same = deck();
    same.to = same.from;
    assert_eq!(build_bridge(&mut w, &same), Err(BridgeRefusal::NoLength));
    let mut gone = deck();
    gone.target = RoomTarget::Existing(9);
    assert_eq!(build_bridge(&mut w, &gone), Err(BridgeRefusal::NoSuchRoom));
}

#[test]
fn the_default_is_a_room_of_its_own_and_a_tree_room_is_refused() {
    let mut w = world();
    let mut p = deck();
    p.target = RoomTarget::New("bridge".into());
    let out = build_bridge(&mut w, &p).unwrap();
    assert_eq!(out.room, 1);
    assert_eq!(w.rooms[1].meshes.len(), 1);
    assert_eq!(w.rooms[1].leaf_count(), 0);
    // A room with a big leaf refuses.
    let mut t = empty();
    let mut r = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [3, 3])),
        "big",
        4096,
    );
    r.leaves = LeafPolicy::Maximal;
    lay_room(&mut t, &r).unwrap();
    let mut q = deck();
    q.target = RoomTarget::Existing(0);
    assert_eq!(build_bridge(&mut t, &q), Err(BridgeRefusal::TreeRoom));
}
