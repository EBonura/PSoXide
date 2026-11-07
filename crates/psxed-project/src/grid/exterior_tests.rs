//! Exterior facing of closure walls, and derived faces addressed by span.

use super::derive::{edge_walls, DerivedKind};
use super::edit::spans::insert_slab_column;
use super::edit::FaceRef;
use super::faces::{all_faces, GridFaceKind};
use super::overrides::ClosureFacing;
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::storeys::SLAB_THICKNESS;
use super::surface::GridSurface;
use super::units::GridScale;
use super::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// One derived cell, 12 clicks tall, alone: every edge is a closure.
fn lone(facing: ClosureFacing) -> GridWorld {
    let mut room = GridRoom::new_derived("box", 1, 1);
    *room.sector_or_insert(0, 0).unwrap() = GridSector::with_surfaces(
        Some(GridSurface::flat(0, Some(rid(1)))),
        Some(GridSurface::flat(3072, Some(rid(2)))),
    );
    room.defaults.closure_facing = facing;
    GridWorld {
        scale: GridScale { num: 1, den: 1 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

#[test]
fn closure_walls_draw_from_outside_only_when_the_room_says_so() {
    for (facing, both) in [(ClosureFacing::Front, false), (ClosureFacing::Both, true)] {
        let world = lone(facing);
        let faces = all_faces(&world);
        let closures: Vec<_> = faces
            .iter()
            .filter(|f| {
                matches!(
                    f.kind,
                    GridFaceKind::Derived {
                        kind: DerivedKind::Closure,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(closures.len(), 4, "{facing:?}");
        assert!(
            closures.iter().all(|f| f.double_sided == both),
            "{facing:?}"
        );
        // Floors and ceilings are never doubled.
        assert!(faces
            .iter()
            .filter(|f| matches!(
                f.kind,
                GridFaceKind::Floor { .. } | GridFaceKind::Ceiling { .. }
            ))
            .all(|f| !f.double_sided));
    }
}

#[test]
fn the_facing_stays_out_of_the_document_until_it_is_set() {
    let text = ron::to_string(&lone(ClosureFacing::Front)).unwrap();
    assert!(!text.contains("closure_facing"), "{text}");
    let text = ron::to_string(&lone(ClosureFacing::Both)).unwrap();
    assert!(text.contains("closure_facing"), "{text}");
    let back: GridWorld = ron::from_str(&text).unwrap();
    assert_eq!(back.rooms[0].defaults.closure_facing, ClosureFacing::Both);
}

/// A column with a slab: two spans, so two closures stand on one edge.
fn two_storeys() -> GridWorld {
    let mut world = lone(ClosureFacing::Front);
    insert_slab_column(
        world.rooms[0].sector_mut(0, 0).unwrap(),
        1536,
        SLAB_THICKNESS,
        None,
    )
    .unwrap();
    world
}

#[test]
fn stacked_faces_of_one_rule_on_an_edge_are_addressed_by_span() {
    let world = two_storeys();
    let room = &world.rooms[0];
    let walls: Vec<_> = edge_walls(room, [0, 0], WallEdge::North)
        .into_iter()
        .filter(|w| w.kind == DerivedKind::Closure)
        .collect();
    assert_eq!(walls.len(), 2, "{walls:#?}");
    assert_eq!(walls.iter().map(|w| w.span).collect::<Vec<_>>(), vec![0, 1]);
    for wall in &walls {
        let face = FaceRef::of_derived(0, wall);
        let found = face.derived_wall(&world).expect("the face resolves");
        assert_eq!(found.span, wall.span);
        assert_eq!(found.heights, wall.heights, "span {}", wall.span);
    }
    // The faces the preview draws carry the same span.
    let faces = all_faces(&world);
    let spans: Vec<u8> = faces
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                GridFaceKind::Derived {
                    edge: WallEdge::North,
                    kind: DerivedKind::Closure
                }
            )
        })
        .map(|f| f.span)
        .collect();
    assert!(spans.contains(&0) && spans.contains(&1), "{spans:?}");
}
