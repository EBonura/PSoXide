//! Tests of Arcade and Colonnade.

use crate::grid::edit::CellRect;
use crate::grid::mesh::{ColumnSpec, MeshKind, Proxy, Socket};
use crate::grid::mesh_arch::ArchProfile;
use crate::grid::overrides::Axis;
use crate::grid::sector::WallEdge;

use super::arcade::{
    build_arcade, build_colonnade, ArcadeParams, ArcadeRefusal, Colonnade, Layout,
};
use super::room::{lay_room, Footprint, RoomParams};
use super::room_tests::empty;

fn hall() -> crate::grid::world::GridWorld {
    let mut w = empty();
    let p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [11, 5])),
        "nave",
        6144,
    );
    lay_room(&mut w, &p).unwrap();
    w
}

#[test]
fn four_bays_share_one_asset_and_keep_the_crown_hook_and_walkable_top() {
    let mut w = hall();
    let before = w.mesh_assets.len();
    let p = ArcadeParams::line(0, [1024, 2048], [1024 + 4 * 2048, 2048], 0, 4);
    let out = build_arcade(&mut w, &p).unwrap();
    assert_eq!(out.instances.len(), 4);
    assert_eq!(w.mesh_assets.len(), before + 1, "one recipe, one asset");
    assert_eq!(out.pitch, 2048);
    assert_eq!(
        out.centres,
        vec![[2048, 2048], [4096, 2048], [6144, 2048], [8192, 2048]]
    );
    let MeshKind::Arch(spec) = &w.mesh_assets[usize::from(out.asset)].kind else {
        panic!("an arch")
    };
    assert_eq!(spec.span, 2048 - 2 * 256, "bays leave room for the piers");
    let built = w.mesh_assets[usize::from(out.asset)].build().unwrap();
    assert!(built
        .sockets
        .iter()
        .any(|s| matches!(s, Socket::Hook { .. })));
    assert!(built
        .proxies
        .iter()
        .any(|p| matches!(p, Proxy::Slab { .. })));
    let room = &w.rooms[0];
    assert_eq!(room.meshes.len(), 4);
    let xs: Vec<i32> = room.meshes.iter().map(|m| m.position[0]).collect();
    assert_eq!(xs, vec![2048, 4096, 6144, 8192]);
}

#[test]
fn an_arcade_is_deterministic() {
    let p = ArcadeParams::line(0, [0, 3072], [8192, 3072], 0, 4);
    let (mut a, mut b) = (hall(), hall());
    build_arcade(&mut a, &p).unwrap();
    build_arcade(&mut b, &p).unwrap();
    assert!(a == b);
}

#[test]
fn overlapping_piers_are_refused_and_nothing_changes() {
    let mut w = hall();
    let before = w.clone();
    let mut p = ArcadeParams::line(0, [0, 2048], [4096, 2048], 0, 4);
    p.span = Some(900);
    assert!(matches!(
        build_arcade(&mut w, &p),
        Err(ArcadeRefusal::PiersOverlap { least_pitch: 1412 })
    ));
    p.span = None;
    p.bays = 0;
    assert_eq!(build_arcade(&mut w, &p), Err(ArcadeRefusal::NothingToBuild));
    let mut q = ArcadeParams::line(7, [0, 0], [1, 0], 0, 1);
    assert_eq!(build_arcade(&mut w, &q), Err(ArcadeRefusal::NoSuchRoom));
    q.room = 0;
    q.pier = 5;
    assert!(matches!(
        build_arcade(&mut w, &q),
        Err(ArcadeRefusal::BadRecipe(_))
    ));
    assert!(w == before);
}

#[test]
fn a_wall_run_and_a_corridor_crossing_lay_out_from_cells() {
    let mut w = hall();
    let mut p = ArcadeParams::line(0, [0, 0], [1, 0], 0, 4);
    p.layout = Layout::Wall {
        first: [0, 0],
        last: [3, 0],
        side: WallEdge::North,
        inset: 512,
    };
    p.profile = ArchProfile::Pointed;
    let out = build_arcade(&mut w, &p).unwrap();
    assert_eq!(out.centres[0], [512, 512]);
    assert_eq!(out.pitch, 1024);
    p.layout = Layout::Across {
        cell: [6, 3],
        along: Axis::X,
        width: 3,
    };
    p.spring = 1024;
    let out = build_arcade(&mut w, &p).unwrap();
    assert_eq!(out.instances.len(), 1);
    let m = w.rooms[0].meshes.last().unwrap();
    assert_eq!(m.position[0], 6 * 1024 + 512);
    assert_eq!(m.rotation[0].rem_euclid(360), 270, "opening runs along z");
}

#[test]
fn a_colonnade_is_the_piers_only() {
    let mut w = hall();
    let c = Colonnade {
        room: 0,
        layout: Layout::Line {
            from: [1024, 2048],
            to: [1024 + 4 * 2048, 2048],
        },
        base: 0,
        bays: 4,
        column: ColumnSpec::default(),
    };
    let out = build_colonnade(&mut w, &c).unwrap();
    assert_eq!(out.instances.len(), 5);
    assert_eq!(out.centres[0], [1024, 2048]);
    assert_eq!(out.centres[4], [9216, 2048]);
    assert!(w.rooms[0].meshes.iter().all(|m| matches!(
        w.mesh_assets[usize::from(m.asset)].kind,
        MeshKind::Column(_)
    )));
}
