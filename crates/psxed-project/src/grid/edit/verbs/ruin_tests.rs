//! Tests of the Ruin pass.

use std::collections::BTreeSet;

use crate::grid::edit::CellRect;
use crate::grid::mesh::MeshKind;
use crate::grid::walk::{analyse, Spawn};
use crate::grid::world::GridWorld;

use super::arcade::{build_arcade, ArcadeParams};
use super::room::{lay_room, Footprint, LeafPolicy, RoomParams};
use super::room_tests::empty;
use super::ruin::{ruin_region, RuinParams, RuinRefusal};

fn nave() -> GridWorld {
    let mut w = empty();
    let mut p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [11, 7])),
        "nave",
        4096,
    );
    p.leaves = LeafPolicy::Sectors;
    lay_room(&mut w, &p).unwrap();
    let a = ArcadeParams::line(0, [1024, 2048], [1024 + 8 * 1024, 2048], 0, 8);
    let mut a = a;
    a.spring = 1024;
    a.pier = 128;
    build_arcade(&mut w, &a).unwrap();
    w
}

fn all() -> BTreeSet<[i32; 2]> {
    CellRect::from_corners([0, 0], [11, 7]).cells().collect()
}

fn spawn() -> Vec<Spawn> {
    vec![Spawn {
        room: 0,
        cell: [0, 4],
    }]
}

fn arches(w: &GridWorld) -> usize {
    w.rooms[0]
        .meshes
        .iter()
        .filter(|m| matches!(w.mesh_assets[usize::from(m.asset)].kind, MeshKind::Arch(_)))
        .count()
}

fn damage(r: &super::ruin::RuinReport) -> usize {
    r.sunk + r.floor_bays + r.roof_bays + r.breaches + r.arches
}

#[test]
fn zero_intensity_changes_nothing_and_a_seed_is_repeatable() {
    let mut w = nave();
    let before = w.clone();
    ruin_region(&mut w, &RuinParams::new(0, all(), 5, 0)).unwrap();
    assert!(w == before);
    let (mut a, mut b, mut c) = (nave(), nave(), nave());
    ruin_region(&mut a, &RuinParams::new(0, all(), 5, 60)).unwrap();
    ruin_region(&mut b, &RuinParams::new(0, all(), 5, 60)).unwrap();
    ruin_region(&mut c, &RuinParams::new(0, all(), 6, 60)).unwrap();
    assert!(a == b, "same seed, same ruin");
    assert!(a != c, "another seed is a re-roll");
    assert!(a != before);
}

#[test]
fn more_intensity_means_more_damage_and_arches_fall_to_their_piers() {
    let mut low = nave();
    let mut high = nave();
    let rl = ruin_region(&mut low, &RuinParams::new(0, all(), 9, 15)).unwrap();
    let rh = ruin_region(&mut high, &RuinParams::new(0, all(), 9, 100)).unwrap();
    assert!(damage(&rh) > damage(&rl), "{rl:?} against {rh:?}");
    assert!(rh.arches > 0 && arches(&high) == 8 - rh.arches);
    // Every fallen arch left two piers (columns) and rubble.
    let columns = high.rooms[0]
        .meshes
        .iter()
        .filter(|m| {
            matches!(
                high.mesh_assets[usize::from(m.asset)].kind,
                MeshKind::Column(_)
            )
        })
        .count();
    assert_eq!(columns, 2 * rh.arches);
    assert_eq!(rh.rubble, rh.arches);
}

#[test]
fn the_keep_mask_is_never_touched_and_the_route_survives() {
    let before = nave();
    let keep: BTreeSet<[i32; 2]> = (0..12).map(|x| [x, 4]).collect();
    let before_walk = analyse(&before, &spawn(), &psx_engine::WalkLimits::DEFAULT).unwrap();
    for seed in 0..6 {
        let mut w = nave();
        let mut p = RuinParams::new(0, all(), seed, 100);
        p.keep = keep.clone();
        p.spawns = spawn();
        ruin_region(&mut w, &p).unwrap();
        for &c in &keep {
            let r0 = &before.rooms[0];
            let r1 = &w.rooms[0];
            let [x, z] = r1.local(c).unwrap();
            let (a, b) = (r0.sector(x, z).unwrap(), r1.sector(x, z).unwrap());
            assert_eq!(
                a.ground_floor().map(|f| f.heights),
                b.ground_floor().map(|f| f.heights),
                "seed {seed} cell {c:?}"
            );
            assert_eq!(
                a.ground_ceiling().is_some(),
                b.ground_ceiling().is_some(),
                "roof bays skip the mask"
            );
        }
        let walk = analyse(&w, &spawn(), &psx_engine::WalkLimits::DEFAULT).unwrap();
        assert!(
            walk.counts()[4] <= before_walk.counts()[4],
            "seed {seed}: no sector is cut off from the spawn"
        );
    }
}

#[test]
fn a_ruin_still_cooks() {
    let mut w = nave();
    let mut p = RuinParams::new(0, all(), 3, 80);
    p.spawns = spawn();
    ruin_region(&mut w, &p).unwrap();
    crate::grid::cook::tests::cook(&w).expect("a ruin cooks");
}

#[test]
fn refusals() {
    let mut w = nave();
    assert_eq!(
        ruin_region(&mut w, &RuinParams::new(3, all(), 1, 10)),
        Err(RuinRefusal::NoSuchRoom)
    );
    assert_eq!(
        ruin_region(&mut w, &RuinParams::new(0, BTreeSet::new(), 1, 10)),
        Err(RuinRefusal::EmptyRegion)
    );
    assert_eq!(
        ruin_region(&mut w, &RuinParams::new(0, all(), 1, 101)),
        Err(RuinRefusal::BadIntensity)
    );
}
