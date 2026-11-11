//! Tests of the shipped bay and of corridors made of bays.

use crate::grid::edit::CellRect;
use crate::grid::leaf::LeafKey;
use crate::grid::world::GridWorld;
use crate::{ProjectDocument, ResourceData, ResourceId};

use super::bay_corridor::{
    built_in_bays, lay_bay_corridor, rib_corridor, BayCorridor, BayRefusal, RIB_CORRIDOR,
};
use super::capture::{capture_bay, BayTemplate};
use super::corridor::Waypoint;
use super::room::RoomTarget;
use super::room_tests::empty;
use super::style::Roles;

fn corridor(bay: &BayTemplate, waypoints: Vec<Waypoint>, width: i32) -> BayCorridor {
    BayCorridor {
        target: RoomTarget::New("rib".into()),
        waypoints,
        width,
        base: 0,
        bay: bay.clone(),
        roles: Roles::default(),
    }
}

/// A straight corridor from `a` to `b` in the shipped bay.
fn straight(a: [i32; 2], b: [i32; 2]) -> (GridWorld, super::bay_corridor::BayOutcome) {
    let mut w = empty();
    let c = corridor(&rib_corridor(), vec![Waypoint::at(a), Waypoint::at(b)], 6);
    let out = lay_bay_corridor(&mut w, &c).unwrap_or_else(|e| panic!("{}", e.describe()));
    (w, out)
}

/// The leaves of `t` as a sorted list, to compare bays that were read in a
/// different order.
fn sorted(t: &BayTemplate) -> Vec<(LeafKey, crate::grid::sector::GridSector)> {
    let mut v = t.leaves.clone();
    v.sort_by_key(|(k, _)| (k.level, k.x, k.z));
    v
}

#[test]
fn the_shipped_bay_loads_with_its_size_and_the_names_of_its_materials() {
    let bays = built_in_bays();
    assert_eq!(bays.len(), 1);
    assert_eq!(bays[0].0, RIB_CORRIDOR);
    let t = &bays[0].1;
    assert_eq!(t.size, [6, 3], "six wide, three long, one rib");
    assert_eq!(t.leaves.len(), 34);
    let names: Vec<&str> = t.materials.iter().map(|(_, n)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "SF / Black panel",
            "SF / Grey plate",
            "SF / Rib flange lit",
            "SF / Rib web lit"
        ]
    );
    // Every id the leaves use is named.
    let text = ron::ser::to_string(t).unwrap();
    let back: BayTemplate = ron::from_str(&text).unwrap();
    assert_eq!(&back, t);
}

#[test]
fn a_bay_turned_four_times_is_itself_and_each_turn_swaps_the_size() {
    let t = rib_corridor();
    let mut cur = t.clone();
    for n in 1..=4 {
        cur = cur.turned(1).unwrap();
        let expect = if n % 2 == 1 { [3, 6] } else { [6, 3] };
        assert_eq!(cur.size, expect, "after {n} turns");
        assert_eq!(cur.leaves.len(), t.leaves.len());
    }
    assert_eq!(sorted(&cur), sorted(&t), "four quarter turns are none");
    assert_eq!(
        sorted(&t.turned(2).unwrap()),
        sorted(&t.turned(1).unwrap().turned(1).unwrap())
    );
}

#[test]
fn a_turned_bay_keeps_the_shape_of_the_floor() {
    let t = rib_corridor();
    // The west cell of the first row slopes down towards the east. Turned a
    // quarter, the same cell is on the south edge and slopes down to the
    // north.
    let west = t
        .leaves
        .iter()
        .find(|(k, _)| k.level == 2 && k.x == 0 && k.z == 0)
        .map(|(_, s)| s.ground_floor().unwrap().heights)
        .unwrap();
    assert!(west[0] > west[1], "{west:?}");
    let q = t.turned(1).unwrap();
    let key = LeafKey::new(2, 0, 20).unwrap();
    let turned = q
        .leaves
        .iter()
        .find(|(k, _)| *k == key)
        .unwrap()
        .1
        .ground_floor()
        .unwrap()
        .heights;
    assert_eq!(turned, [west[1], west[2], west[3], west[0]]);
    assert!(turned[0] < turned[3], "lower to the north: {turned:?}");
}

#[test]
fn twelve_sectors_of_corridor_are_four_bays_cell_for_cell() {
    let (w, out) = straight([2, 0], [2, 11]);
    assert_eq!(out.bays, 4);
    assert_eq!(out.plain, 0);
    let t = rib_corridor();
    for n in 0..4 {
        // The footprint is x 0..=5 (centre line 2, six wide), rows by 3.
        let copy = capture_bay(
            &w,
            out.room,
            CellRect::from_corners([0, 3 * n], [5, 3 * n + 2]),
        )
        .unwrap();
        assert_eq!(copy.size, t.size);
        assert_eq!(sorted(&copy), sorted(&t), "bay {n} equals the source");
    }
    assert!(crate::grid::tree::check(&w.rooms[out.room]).is_empty());
    crate::grid::cook::tests::cook(&w).expect("the corridor cooks");
}

#[test]
fn every_heading_lays_the_same_bays_turned() {
    let t = rib_corridor();
    for (a, b, turns, origin) in [
        ([2, 0], [2, 5], 0u8, [0, 0]),
        ([0, 2], [5, 2], 1, [0, 0]),
        ([2, 5], [2, 0], 2, [0, 0]),
        ([5, 2], [0, 2], 3, [0, 0]),
    ] {
        let (w, out) = straight(a, b);
        assert_eq!(out.bays, 2, "{a:?} to {b:?}");
        let want = t.turned(turns).unwrap();
        let rect = CellRect::from_corners(
            origin,
            [origin[0] + want.size[0] - 1, origin[1] + want.size[1] - 1],
        );
        let first = capture_bay(&w, out.room, rect).unwrap();
        assert_eq!(sorted(&first), sorted(&want), "{a:?} to {b:?}");
        crate::grid::cook::tests::cook(&w).unwrap_or_else(|e| panic!("{a:?} to {b:?}: {e}"));
    }
}

#[test]
fn a_height_is_the_corridors_and_a_width_is_the_bays() {
    let t = rib_corridor();
    let mut w = empty();
    let before = w.clone();
    let wrong = corridor(&t, vec![Waypoint::at([0, 0]), Waypoint::at([0, 8])], 2);
    let e = lay_bay_corridor(&mut w, &wrong).unwrap_err();
    assert_eq!(e, BayRefusal::WrongWidth { want: 6, got: 2 });
    assert!(e.describe().contains("6 sectors wide"), "{}", e.describe());
    let rising = corridor(
        &t,
        vec![
            Waypoint::at_height([0, 0], 0),
            Waypoint::at_height([0, 8], 256),
        ],
        6,
    );
    assert_eq!(
        lay_bay_corridor(&mut w, &rising).unwrap_err(),
        BayRefusal::NotLevel
    );
    let short = corridor(&t, vec![Waypoint::at([0, 0]), Waypoint::at([0, 1])], 6);
    assert_eq!(
        lay_bay_corridor(&mut w, &short).unwrap_err(),
        BayRefusal::TooShort { bay: 3 }
    );
    assert_eq!(w, before, "a refusal leaves the document alone");
}

#[test]
fn rows_that_do_not_fill_a_bay_stay_plain_at_the_far_end() {
    // Eight rows: two bays and two plain rows.
    let (w, out) = straight([2, 0], [2, 7]);
    assert_eq!(out.bays, 2);
    assert_eq!(out.plain, 12, "two rows of six");
    let room = &w.rooms[out.room];
    for z in 6..8 {
        let (key, s) = room
            .leaf_at([0, z * 4])
            .map(|(k, s)| (k, s.clone()))
            .unwrap();
        assert!(key.is_sector(), "row {z} is whole sectors");
        let f = s.ground_floor().unwrap().heights;
        assert_eq!(f, [0; 4], "flat landing");
    }
}

#[test]
fn a_turn_keeps_a_plain_landing_and_both_legs_get_bays() {
    let mut w = empty();
    let c = corridor(
        &rib_corridor(),
        vec![
            Waypoint::at([0, 0]),
            Waypoint::at([0, 14]),
            Waypoint::at([14, 14]),
        ],
        6,
    );
    let out = lay_bay_corridor(&mut w, &c).unwrap_or_else(|e| panic!("{}", e.describe()));
    assert!(out.bays >= 2, "{out:?}");
    // The corner square of the waypoint is whole, flat sectors.
    let room = &w.rooms[out.room];
    for cell in [[0, 14], [1, 13], [2, 12]] {
        let (key, s) = room.leaf_at([cell[0] * 4, cell[1] * 4]).unwrap();
        assert!(key.is_sector(), "{cell:?}");
        assert_eq!(s.ground_floor().unwrap().heights, [0; 4], "{cell:?}");
    }
    assert!(crate::grid::tree::check(room).is_empty());
    crate::grid::cook::tests::cook(&w).expect("the bent corridor cooks");
}

fn project_with(names: &[&str]) -> (ProjectDocument, Vec<ResourceId>) {
    let mut p = ProjectDocument::new("bind");
    let mut ids = Vec::new();
    for n in names {
        let id = p.add_resource(
            *n,
            ResourceData::Material(crate::MaterialResource::opaque(None)),
        );
        ids.push(id);
    }
    (p, ids)
}

#[test]
fn a_bay_binds_to_the_project_by_name_and_falls_back_to_its_roles() {
    let bay = rib_corridor();
    // A project that has two of the four materials, by name, and roles.
    let (p, ids) = project_with(&[
        "SF / Grey plate",
        "SF / Rib flange lit",
        "Floor",
        "Wall",
        "Ceiling",
    ]);
    let roles = Roles {
        floor: Some(ids[2]),
        wall: Some(ids[3]),
        ceiling: Some(ids[4]),
        ..Roles::default()
    };
    let bound = bay.bound_to(&p, roles);
    assert!(bound.materials.is_empty());
    let mut used: std::collections::HashSet<ResourceId> = std::collections::HashSet::new();
    for (_, s) in &bound.leaves {
        for surf in s.surfaces() {
            used.extend(surf.tri_a.material);
        }
        used.extend(s.ground_wall_material());
    }
    // Grey plate and Rib flange by name; Black panel falls back to the floor
    // or ceiling role by where it sits; Rib web falls back to the wall role.
    assert!(used.contains(&ids[0]) && used.contains(&ids[1]));
    assert!(used.contains(&ids[3]), "wall role");
    for id in &used {
        assert!(ids.contains(id), "every material is the project's");
    }
    // A project with none of them and no roles gets untextured cells.
    let (bare, _) = project_with(&["Other"]);
    let none = bay.bound_to(&bare, Roles::default());
    assert!(none
        .leaves
        .iter()
        .all(|(_, s)| s.surfaces().all(|f| f.tri_a.material.is_none())));
}

#[test]
fn naming_a_captured_bays_materials_records_what_the_project_calls_them() {
    let (p, ids) = project_with(&["Stone", "Brick"]);
    let mut w = empty();
    let rect = CellRect::from_corners([0, 0], [1, 1]);
    let r = super::room::RoomParams::new(super::room::Footprint::Rect(rect), "", 2048);
    let mut r = r;
    r.floor_material = Some(ids[0]);
    r.ceiling_material = Some(ids[1]);
    super::room::lay_room(&mut w, &r).unwrap();
    let mut bay = capture_bay(&w, 0, rect).unwrap();
    assert!(bay.materials.is_empty());
    bay.name_materials(&p);
    let names: Vec<&str> = bay.materials.iter().map(|(_, n)| n.as_str()).collect();
    assert_eq!(names, ["Stone", "Brick"]);
}
