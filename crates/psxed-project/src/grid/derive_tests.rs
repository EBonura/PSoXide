//! Derived walls on hand-built worlds: platform, pit, bow tie, closure,
//! the material chain and overrides.

use super::derive::{cell_walls, edge_key, physical_edge_walls, DerivedKind};
use super::faces::{all_faces, GridFace, GridFaceKind};
use super::overrides::WallMode;
use super::room::GridRoom;
use super::sector::WallEdge;
use super::surface::GridSurface;
use super::units::GridScale;
use super::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A derived `side` by `side` room with a flat floor at 0 and a flat ceiling
/// at 2048 everywhere.
fn hall(side: usize) -> GridRoom {
    let mut room = GridRoom::new_derived("hall", side, side);
    for z in 0..side {
        for x in 0..side {
            let s = room.sector_or_insert(x, z).unwrap();
            s.set_floor(Some(GridSurface::flat(0, Some(rid(1)))));
            s.set_ceiling(Some(GridSurface::flat(2048, Some(rid(2)))));
        }
    }
    room
}

fn world(room: GridRoom) -> GridWorld {
    GridWorld {
        scale: GridScale { num: 1, den: 1 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn derived(faces: &[GridFace], kind: DerivedKind) -> Vec<&GridFace> {
    faces
        .iter()
        .filter(|f| matches!(f.kind, GridFaceKind::Derived { kind: k, .. } if k == kind))
        .collect()
}

fn y_span(face: &GridFace) -> (i32, i32) {
    (
        face.points.iter().map(|p| p[1]).min().unwrap(),
        face.points.iter().map(|p| p[1]).max().unwrap(),
    )
}

#[test]
fn a_closed_hall_gets_only_closure_walls_around_its_outline() {
    let faces = all_faces(&world(hall(3)));
    assert!(derived(&faces, DerivedKind::Riser).is_empty());
    assert!(derived(&faces, DerivedKind::Drop).is_empty());
    // 12 outline edges, each one face however tall (derived walls are not
    // cut into courses).
    let closure = derived(&faces, DerivedKind::Closure);
    assert_eq!(closure.len(), 12);
    for face in closure {
        // Seen from inside: the normal points towards the room centre
        // (1.5 sectors, 1.5 sectors).
        let c = face.points.iter().fold([0.0; 2], |a, p| {
            [a[0] + f64::from(p[0]) / 4.0, a[1] + f64::from(p[2]) / 4.0]
        });
        let to_centre = [1536.0 - c[0], 1536.0 - c[1]];
        assert!(face.normal[0] * to_centre[0] + face.normal[2] * to_centre[1] > 0.0);
    }
}

#[test]
fn a_raised_platform_grows_four_risers_seen_from_the_lower_side() {
    let mut room = hall(3);
    room.sector_or_insert(1, 1)
        .unwrap()
        .set_floor(Some(GridSurface::flat(512, Some(rid(3)))));
    let faces = all_faces(&world(room));
    let risers = derived(&faces, DerivedKind::Riser);
    assert_eq!(risers.len(), 4);
    for face in &risers {
        assert_eq!(y_span(face), (0, 512));
        // Each faces away from the platform centre (1536, 1536).
        let c = face.points.iter().fold([0.0; 2], |a, p| {
            [a[0] + f64::from(p[0]) / 4.0, a[1] + f64::from(p[2]) / 4.0]
        });
        let away = [c[0] - 1536.0, c[1] - 1536.0];
        assert!(
            face.normal[0] * away[0] + face.normal[2] * away[1] > 0.0,
            "{face:?}"
        );
        // Hosted by a neighbour of the platform, not the platform.
        assert_ne!(face.cell, [1, 1]);
    }
    // The chain falls to the owner's floor material (the platform's).
    assert!(risers.iter().all(|f| f.material == Some(rid(3))));
}

#[test]
fn a_pit_grows_four_risers_seen_from_inside_it() {
    let mut room = hall(3);
    room.sector_or_insert(1, 1)
        .unwrap()
        .set_floor(Some(GridSurface::flat(-512, Some(rid(3)))));
    let faces = all_faces(&world(room));
    let risers = derived(&faces, DerivedKind::Riser);
    assert_eq!(risers.len(), 4);
    for face in &risers {
        assert_eq!(y_span(face), (-512, 0));
        assert_eq!(face.cell, [1, 1], "hosted by the pit");
        let c = face.points.iter().fold([0.0; 2], |a, p| {
            [a[0] + f64::from(p[0]) / 4.0, a[1] + f64::from(p[2]) / 4.0]
        });
        let into_pit = [1536.0 - c[0], 1536.0 - c[1]];
        assert!(face.normal[0] * into_pit[0] + face.normal[2] * into_pit[1] > 0.0);
    }
}

#[test]
fn a_lower_ceiling_drops_towards_the_higher_side() {
    let mut room = hall(3);
    room.sector_or_insert(1, 1)
        .unwrap()
        .set_ceiling(Some(GridSurface::flat(1024, None)));
    let faces = all_faces(&world(room));
    let drops = derived(&faces, DerivedKind::Drop);
    assert_eq!(drops.len(), 4);
    assert!(drops.iter().all(|f| y_span(f) == (1024, 2048)));
    assert!(drops.iter().all(|f| f.cell != [1, 1]));
}

#[test]
fn crossing_floors_cut_a_bow_tie_into_two_triangles_with_opposite_owners() {
    // Two cells side by side; along their shared edge the left floor goes
    // 0 -> 512 and the right one 512 -> 0 (north to south).
    let mut room = GridRoom::new_derived("bow", 2, 1);
    for x in 0..2 {
        room.sector_or_insert(x, 0)
            .unwrap()
            .set_floor(Some(GridSurface::flat(0, None)));
    }
    // East edge of cell 0 runs corners NE (1) -> SE (2).
    room.sector_or_insert(0, 0)
        .unwrap()
        .floor_mut()
        .unwrap()
        .heights = [0, 0, 512, 512];
    // West edge of cell 1 runs SW (3) -> NW (0): 0 in the south, 512 in the
    // north, so against cell 0 (0 in the north, 512 in the south) it crosses.
    room.sector_or_insert(1, 0)
        .unwrap()
        .floor_mut()
        .unwrap()
        .heights = [512, 0, 0, 0];
    let w = world(room);
    let walls = physical_edge_walls(&w.rooms[0], [0, 0], WallEdge::East);
    let risers: Vec<_> = walls
        .iter()
        .filter(|d| d.kind == DerivedKind::Riser)
        .collect();
    assert_eq!(risers.len(), 2, "{walls:?}");
    assert_ne!(risers[0].host, risers[1].host);
    assert_ne!(risers[0].owner, risers[1].owner);
    assert!(risers
        .iter()
        .all(|d| d.ring.len() == 3 && d.heights.is_none()));
    // The two triangles meet at the crossing, halfway along the edge.
    let apex = |d: &&super::derive::DerivedWall| {
        d.ring
            .iter()
            .copied()
            .find(|p| p[0] != 0 && p[0] != 1024)
            .unwrap()
    };
    let (a, b) = (apex(&risers[0]), apex(&risers[1]));
    assert_eq!(a[0] + b[0], 1024, "apexes are mirrored in host terms");
    assert_eq!(a[1], b[1]);
    assert_eq!(a[1], 256);
}

#[test]
fn equal_profiles_and_floorless_neighbours_make_no_wall() {
    let mut room = hall(2);
    // A pit with no floor at all: no riser, no closure.
    room.sector_or_insert(1, 1).unwrap().set_floor(None);
    let faces = all_faces(&world(room));
    assert!(derived(&faces, DerivedKind::Riser).is_empty());
    // Sloped neighbours that meet at the same heights along the edge: a
    // ridge across two cells whose shared edge is level at 256.
    let mut room = GridRoom::new_derived("ridge", 2, 1);
    for x in 0..2 {
        room.sector_or_insert(x, 0)
            .unwrap()
            .set_floor(Some(GridSurface::flat(0, None)));
    }
    room.sector_or_insert(0, 0)
        .unwrap()
        .floor_mut()
        .unwrap()
        .heights = [0, 256, 256, 0];
    room.sector_or_insert(1, 0)
        .unwrap()
        .floor_mut()
        .unwrap()
        .heights = [256, 0, 0, 256];
    let faces = all_faces(&world(room));
    assert!(derived(&faces, DerivedKind::Riser).is_empty());
}

#[test]
fn a_floor_without_a_ceiling_closes_only_when_the_room_asks() {
    let mut room = GridRoom::new_derived("yard", 2, 1);
    for x in 0..2 {
        room.sector_or_insert(x, 0)
            .unwrap()
            .set_floor(Some(GridSurface::flat(0, None)));
    }
    assert!(derived(&all_faces(&world(room.clone())), DerivedKind::Closure).is_empty());
    room.defaults.open_wall = 512;
    let faces = all_faces(&world(room));
    let closure = derived(&faces, DerivedKind::Closure);
    assert_eq!(closure.len(), 6, "2 cells x 3 outline edges");
    assert!(closure.iter().all(|f| y_span(f) == (0, 512)));
}

#[test]
fn the_material_chain_runs_override_owner_facing_room_then_surface() {
    let mut room = hall(3);
    room.sector_or_insert(1, 1)
        .unwrap()
        .set_floor(Some(GridSurface::flat(512, Some(rid(3)))));
    let host = [1, 0];
    let first = |room: &GridRoom| {
        cell_walls(room, host)
            .into_iter()
            .find(|d| d.kind == DerivedKind::Riser)
            .unwrap()
            .material
    };
    // 5: the owner's own floor.
    assert_eq!(first(&room), Some(rid(3)));
    // 4: the room default.
    room.defaults.wall_material = Some(rid(4));
    assert_eq!(first(&room), Some(rid(4)));
    // 3: the facing sector's wall material.
    room.sector_or_insert(1, 0)
        .unwrap()
        .set_wall_material(Some(rid(5)));
    assert_eq!(first(&room), Some(rid(5)));
    // 2: the owner's wall material.
    room.sector_or_insert(1, 1)
        .unwrap()
        .set_wall_material(Some(rid(6)));
    assert_eq!(first(&room), Some(rid(6)));
    // 1: the override.
    let key = edge_key(host, WallEdge::South);
    room.edge_overrides.edit(key, |o| o.material = Some(rid(7)));
    assert_eq!(first(&room), Some(rid(7)));
}

#[test]
fn hidden_overrides_remove_the_face_and_authored_rooms_derive_nothing() {
    let mut room = hall(3);
    room.sector_or_insert(1, 1)
        .unwrap()
        .set_floor(Some(GridSurface::flat(512, None)));
    let before = all_faces(&world(room.clone())).len();
    let key = edge_key([1, 0], WallEdge::South);
    room.edge_overrides.edit(key, |o| o.hidden = true);
    // One riser of 512 is one face.
    assert_eq!(all_faces(&world(room.clone())).len(), before - 1);
    room.wall_mode = WallMode::Authored;
    let authored = all_faces(&world(room));
    assert!(authored
        .iter()
        .all(|f| !matches!(f.kind, GridFaceKind::Derived { .. })));
}

#[test]
fn both_sectors_of_an_edge_agree_on_its_key_and_faces() {
    let mut room = hall(2);
    room.sector_or_insert(1, 0)
        .unwrap()
        .set_floor(Some(GridSurface::flat(256, None)));
    assert_eq!(
        edge_key([0, 0], WallEdge::East),
        edge_key([1, 0], WallEdge::West)
    );
    let from_west = physical_edge_walls(&room, [0, 0], WallEdge::East);
    let from_east = physical_edge_walls(&room, [1, 0], WallEdge::West);
    assert_eq!(from_west, from_east);
}

#[test]
fn an_authored_wall_on_top_of_a_derived_face_is_flagged_but_still_cooks() {
    use super::limits::{validate, GridIssueKind};
    use super::surface::GridWall;
    let mut room = hall(3);
    room.sector_or_insert(1, 1)
        .unwrap()
        .set_floor(Some(GridSurface::flat(512, None)));
    let w = world(room.clone());
    assert!(validate(&w)
        .iter()
        .all(|i| !matches!(i.kind, GridIssueKind::AuthoredDuplicatesDerived { .. })));
    // The same wall the riser already is, authored on the lower side.
    room.sector_or_insert(1, 0)
        .unwrap()
        .walls_mut(WallEdge::South)
        .push(GridWall::span(0, 512, None));
    let issues = validate(&world(room.clone()));
    assert!(issues
        .iter()
        .any(|i| matches!(i.kind, GridIssueKind::AuthoredDuplicatesDerived { .. })));
    assert!(!super::limits::has_errors(&issues));
    // A wall that rises above the riser is not a duplicate of it.
    let mut room = hall(3);
    room.sector_or_insert(1, 1)
        .unwrap()
        .set_floor(Some(GridSurface::flat(512, None)));
    room.sector_or_insert(1, 0)
        .unwrap()
        .walls_mut(WallEdge::South)
        .push(GridWall::span(512, 1536, None));
    assert!(validate(&world(room))
        .iter()
        .all(|i| !matches!(i.kind, GridIssueKind::AuthoredDuplicatesDerived { .. })));
}
