//! The leaf storage: disk form negotiation, the accessors and the span
//! column (T8). Everything here is about the document model; no behaviour
//! of the tools changed, which the older tests prove by passing untouched.

use super::edit::{FaceRef, FaceTarget, SurfaceKind};
use super::leaf::LeafKey;
use super::room::{GridRoom, LeafStoreError};
use super::sector::{GridSector, GridSpan};
use super::surface::{GridSurface, GridWall};
use super::units::GridScale;
use super::world::GridWorld;

const PROJECTS: [&str; 9] = [
    "grid-cathedral",
    "grid-level008",
    "grid-dungeon",
    "grid-cave",
    "grid-sewers",
    "grid-west",
    "grid-planes",
    "grid-gate",
    "grid-starter",
];

fn load(name: &str) -> crate::ProjectDocument {
    let path = crate::projects_dir().join(name).join("project.ron");
    crate::ProjectDocument::load_from_path(path).expect("project loads")
}

fn ron_of<T: serde::Serialize>(value: &T) -> String {
    ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::new().depth_limit(4)).unwrap()
}

fn key(level: u8, x: i32, z: i32) -> LeafKey {
    LeafKey::new(level, x, z).unwrap()
}

fn flat(height: i32) -> GridSector {
    GridSector::with_surfaces(Some(GridSurface::flat(height, None)), None)
}

fn span(floor: i32, ceiling: i32) -> GridSpan {
    GridSpan {
        floor: Some(GridSurface::flat(floor, None)),
        ceiling: Some(GridSurface::flat(ceiling, None)),
        wall_material: None,
    }
}

#[test]
fn the_reference_projects_load_dense_and_round_trip_as_documents() {
    for name in PROJECTS {
        let project = load(name);
        let world = project.active_scene().world.grid().expect("a grid project");
        for room in &world.rooms {
            assert!(room.is_uniform(), "{name}: every leaf is a whole sector");
            assert!(room.leaves().all(|(_, s)| s.is_single_span()), "{name}");
        }
        let first = project.to_ron_string().unwrap();
        assert!(
            !first.contains("leaves:") && !first.contains("above:"),
            "{name}"
        );
        let again = crate::ProjectDocument::from_ron_str(&first).unwrap();
        assert_eq!(
            again.active_scene().world,
            project.active_scene().world,
            "{name}: the document survives a round trip"
        );
        assert_eq!(
            again.to_ron_string().unwrap(),
            first,
            "{name}: and is stable"
        );
    }
}

fn mixed_room() -> GridRoom {
    let mut room = GridRoom::new("mixed", 8, 8);
    room.origin = [-4, 0];
    // One block of four sectors, a pair of half sectors and a quarter, whole
    // sectors beside them, and a hole.
    room.insert_leaf(key(0, -16, 0), flat(0)).unwrap();
    room.insert_leaf(key(3, 0, 0), flat(256)).unwrap();
    room.insert_leaf(key(3, 2, 0), flat(512)).unwrap();
    room.insert_leaf(key(4, 0, 2), flat(768)).unwrap();
    room.insert_leaf(LeafKey::sector([1, 1]), flat(1024))
        .unwrap();
    room.insert_leaf(LeafKey::sector([2, 0]), GridSector::default())
        .unwrap();
    room
}

#[test]
fn a_room_with_other_leaves_writes_the_leaf_form_and_loads_back() {
    let room = mixed_room();
    assert!(!room.is_uniform());
    let text = ron_of(&room);
    assert!(
        text.contains("leaves:") && !text.contains("sectors:"),
        "{text}"
    );
    let back: GridRoom = ron::from_str(&text).unwrap();
    assert_eq!(back, room);
    assert_eq!(ron_of(&back), text);
    let keys: Vec<LeafKey> = back.leaves().map(|(k, _)| k).collect();
    let mut sorted = keys.clone();
    sorted.sort_by_key(|k| k.order());
    assert_eq!(keys, sorted, "Morton order");
}

#[test]
fn extra_spans_ride_the_dense_form_and_a_single_span_is_written_as_before() {
    let mut room = GridRoom::new("tall", 2, 1);
    room.sector_or_insert(0, 0)
        .unwrap()
        .set_ground_floor(Some(GridSurface::flat(0, None)));
    let mut hall = GridSector::default();
    hall.insert_span(0, span(0, 2048));
    hall.insert_span(1, span(3072, 5120));
    hall.remove_span(2);
    assert_eq!(hall.span_count(), 2);
    room.set_sector(1, 0, Some(hall));
    assert!(room.is_uniform());
    let text = ron_of(&room);
    assert!(
        text.contains("sectors:") && text.contains("above:"),
        "{text}"
    );
    assert_eq!(
        text.matches("above:").count(),
        1,
        "only the tall column has it"
    );
    let back: GridRoom = ron::from_str(&text).unwrap();
    assert_eq!(back, room);
    assert_eq!(back.sector(1, 0).unwrap().span_count(), 2);
    assert!(back.sector(0, 0).unwrap().is_single_span());
}

#[test]
fn a_dense_list_of_the_wrong_length_reads_as_far_as_it_goes() {
    let mut room = GridRoom::new("r", 2, 2);
    for (x, z) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        room.sector_or_insert(x, z)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(0, None)));
    }
    let text = ron_of(&room);
    // Declare a deeper room than the list holds: the tail is empty sectors.
    let taller = text.replace("depth: 2", "depth: 3");
    let back: GridRoom = ron::from_str(&taller).unwrap();
    assert_eq!(back.sector_count(), 4);
    assert!(back.sector(0, 2).is_none());
    // Declare a shallower room: the extra entries lie outside it, where
    // validation reports them.
    let shorter = text.replace("depth: 2", "depth: 1");
    let back: GridRoom = ron::from_str(&shorter).unwrap();
    assert_eq!(back.sector_count(), 4);
    assert!(!back.is_uniform());
}

#[test]
fn sector_accessors_see_whole_sectors_only_and_never_overlap_other_leaves() {
    let mut room = mixed_room();
    // Local (0, 0) is world sector (-4, 0): the level-0 block holds it.
    assert!(room.sector(0, 0).is_none());
    assert!(room.sector_or_insert(0, 0).is_none());
    // Local (4, 0) is world sector (0, 0): three finer leaves sit in it.
    assert!(room.sector(4, 0).is_none());
    assert!(room.sector_or_insert(4, 0).is_none());
    assert!(room.set_sector(4, 0, Some(GridSector::default())).is_none());
    assert_eq!(room.leaf_count(), 6, "nothing was stored");
    // A free cell takes a sector.
    assert!(room.sector_or_insert(7, 7).is_some());
    assert_eq!(room.leaf_count(), 7);
    // Any-size lookup by click.
    assert_eq!(room.leaf_at([0, 0]).unwrap().0, key(3, 0, 0));
    assert_eq!(room.leaf_at([3, 1]).unwrap().0, key(3, 2, 0));
    assert_eq!(room.leaf_at([0, 2]).unwrap().0, key(4, 0, 2));
    assert_eq!(room.leaf_at([-1, 15]).unwrap().0, key(0, -16, 0));
    assert!(room.leaf_at([1, 2]).is_none());
    // The adapter lists whole sectors only, in the old z-major order.
    let listed: Vec<[usize; 2]> = room.iter().map(|(x, z, _)| [x, z]).collect();
    assert_eq!(listed, vec![[6, 0], [5, 1], [7, 7]]);
    assert_eq!(room.sector_count(), 3);
}

#[test]
fn leaves_must_be_inside_the_room_and_disjoint() {
    let mut room = mixed_room();
    assert_eq!(
        room.insert_leaf(LeafKey::sector([10, 0]), GridSector::default()),
        Err(LeafStoreError::OutsideRoom(LeafKey::sector([10, 0])))
    );
    // A block that sticks out of the rectangle.
    let edge = key(0, 16, 16);
    assert!(matches!(
        room.insert_leaf(edge, GridSector::default()),
        Err(LeafStoreError::OutsideRoom(_))
    ));
    assert!(matches!(
        room.insert_leaf(key(1, 0, 0), GridSector::default()),
        Err(LeafStoreError::Leaf(_))
    ));
    assert!(room.remove_leaf(key(3, 0, 0)).is_some());
    assert_eq!(room.leaf_count(), 5);
}

#[test]
fn translating_a_room_moves_the_origin_and_every_leaf_together() {
    let mut room = GridRoom::new("r", 3, 1);
    room.origin = [2, 2];
    room.sector_or_insert(1, 0)
        .unwrap()
        .set_ground_floor(Some(GridSurface::flat(64, None)));
    let before = room.sector(1, 0).cloned();
    assert!(room.translate([5, -3]));
    assert_eq!(room.origin, [7, -1]);
    assert_eq!(
        room.sector(1, 0).cloned(),
        before,
        "the same cell of the room"
    );
    assert_eq!(room.sector_key(1, 0), Some(LeafKey::sector([8, -1])));
    // Assigning the origin alone does not carry the leaves.
    let mut other = room.clone();
    other.origin[0] += 1;
    assert!(other.sector(1, 0).is_none());
    // A block of four sectors only moves by whole blocks.
    let mut mixed = mixed_room();
    assert!(!mixed.translate([1, 0]));
    assert_eq!(mixed, super::leaf_tests::mixed_room());
    assert!(mixed.translate([4, 4]));
    assert!(mixed.leaves().all(|(k, _)| mixed.contains_leaf(k)));
}

#[test]
fn growing_and_trimming_never_move_a_leaf() {
    let mut room = mixed_room();
    let before: Vec<LeafKey> = room.leaves().map(|(k, _)| k).collect();
    room.grow_to_include(-3, -3);
    room.grow_to_include(20, 20);
    assert_eq!(room.leaves().map(|(k, _)| k).collect::<Vec<_>>(), before);
    room.trim_empty();
    // The empty placeholder at sector (2, 0) went; the rest is kept whole,
    // and the rectangle is the bounds of the leaves, the block included.
    let kept: Vec<LeafKey> = room.leaves().map(|(k, _)| k).collect();
    assert_eq!(kept.len(), before.len() - 1);
    assert!(!kept.contains(&LeafKey::sector([2, 0])));
    assert!(kept.iter().all(|k| room.contains_leaf(*k)));
    assert_eq!((room.origin, room.width, room.depth), ([-4, 0], 6, 4));
}

#[test]
fn a_face_ref_names_the_leaf_and_resolves_any_size() {
    let floor = FaceRef::floor(0, [3, -2]);
    assert_eq!(floor.leaf, LeafKey::sector([3, -2]));
    assert_eq!(floor.cell(), [3, -2]);
    let mut world = GridWorld {
        scale: GridScale { num: 1, den: 1 },
        rooms: vec![mixed_room()],
        ..GridWorld::default()
    };
    // The floor of the quarter-sector leaf resolves through its key.
    let quarter = FaceRef::new(0, key(4, 0, 2), FaceTarget::ground(SurfaceKind::Floor));
    assert_eq!(quarter.cell(), [0, 0]);
    assert_eq!(quarter.surface(&world).unwrap().heights, [768; 4]);
    quarter.surface_mut(&mut world).unwrap().heights = [1; 4];
    assert_eq!(quarter.heights(&world, 0), Some([1; 4]));
    // Its corners sit on the click lattice: leaf corner plus one edge.
    let corners = quarter.corners(&world);
    let at = |c: usize| corners[c].grid_pos(&world).unwrap();
    assert_eq!(at(0), [0, 1, 2 * 256]);
    assert_eq!(at(2), [256, 1, 3 * 256]);
    // A whole sector keeps the old arithmetic.
    let sector = FaceRef::floor(0, [1, 1]);
    assert_eq!(sector.corners(&world)[2].grid_pos(&world).unwrap()[0], 2048);
    // A reference to a leaf that is not there does not exist.
    assert!(!FaceRef::new(0, key(4, 3, 3), FaceTarget::ground(SurfaceKind::Floor)).exists(&world));
}

#[test]
fn the_span_resolver_picks_the_storey_a_height_is_in() {
    // Ground hall 0..2048, a slab, an upper hall 3072..5120, open sky above.
    let mut column = GridSector::default();
    column.insert_span(0, span(0, 2048));
    column.insert_span(
        1,
        GridSpan {
            floor: Some(GridSurface::flat(3072, None)),
            ceiling: None,
            wall_material: None,
        },
    );
    column.remove_span(2);
    assert_eq!(column.span_count(), 2);
    assert_eq!(column.span_at(100, 0), Some(0));
    assert_eq!(column.span_at(2047, 0), Some(0));
    assert_eq!(column.span_at(2500, 0), None, "inside the slab");
    assert_eq!(column.span_at(3072, 0), Some(1));
    assert_eq!(column.span_at(900_000, 0), Some(1), "open sky");
    // The step-up reach: the first tread belongs to the upper storey from
    // below, and the ground hall still holds a height under its ceiling.
    assert_eq!(column.span_at(3000, 100), Some(1));
    assert_eq!(column.span_at(3000, 0), None);
    assert_eq!(
        column.span_at(1500, 2000),
        Some(1),
        "the highest reachable floor wins"
    );
    // The ground span is what the single-span accessors read.
    assert_eq!(column.ground_floor().unwrap().heights[0], 0);
    assert_eq!(column.surfaces().count(), 3);
    assert_eq!(column.height_range(), Some((0, 3072)));
    // A pit has no floor and always reaches; the last span cannot go.
    let pit = GridSector::with_surfaces(None, Some(GridSurface::flat(512, None)));
    assert_eq!(pit.span_at(-100_000, 0), Some(0));
    assert_eq!(pit.span_at(512, 0), None);
    let mut single = GridSector::default();
    assert!(single.remove_span(0).is_some());
    assert_eq!(single.span_count(), 1, "a column is never empty");
}

#[test]
fn the_column_query_of_the_world_reads_every_span_and_every_leaf_size() {
    let mut room = GridRoom::new("r", 2, 1);
    let mut tall = GridSector::default();
    tall.insert_span(0, span(0, 1024));
    tall.insert_span(1, span(2048, 3072));
    tall.remove_span(2);
    room.set_sector(0, 0, Some(tall));
    // A half-sector leaf in the second sector.
    room.insert_leaf(key(3, 4, 0), flat(300)).unwrap();
    let world = GridWorld {
        scale: GridScale { num: 1, den: 1 },
        rooms: vec![room],
        ..GridWorld::default()
    };
    assert_eq!(world.floor_height_at(500, 500, 10_000), Some(2048));
    assert_eq!(world.floor_height_at(500, 500, 1000), Some(0));
    assert_eq!(world.floor_height_at(500, 500, -1), None);
    // Sector 1 holds a leaf of half a sector: x 1024..1536.
    assert_eq!(world.floor_height_at(1100, 100, 10_000), Some(300));
    assert_eq!(world.floor_height_at(1600, 100, 10_000), None);
}

#[test]
fn a_column_keeps_its_walls_beside_the_spans() {
    let mut sector = flat(0);
    sector.north.push(GridWall::span(0, 1024, None));
    sector.insert_span(1, span(2048, 3072));
    let room = {
        let mut r = GridRoom::new("r", 1, 1);
        r.set_sector(0, 0, Some(sector.clone()));
        r
    };
    let back: GridRoom = ron::from_str(&ron_of(&room)).unwrap();
    assert_eq!(back.sector(0, 0), Some(&sector));
    assert_eq!(back.sector(0, 0).unwrap().north.len(), 1);
}

/// `cargo test -p psxed-project --release --lib measure_leaf_storage -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn measure_leaf_storage_on_the_cathedral() {
    use std::time::Instant;
    let project = load("grid-cathedral");
    let world = project.active_scene().world.grid().unwrap();
    let room = &world.rooms[0];
    let (w, d) = (room.width, room.depth);
    let rounds = 2000;
    let t = Instant::now();
    let mut hits = 0usize;
    for _ in 0..rounds {
        for z in 0..d {
            for x in 0..w {
                hits += usize::from(room.sector(x, z).is_some());
            }
        }
    }
    let per = t.elapsed().as_nanos() as f64 / (rounds * w * d) as f64;
    eprintln!("sector lookup: {per:.1} ns each ({hits} hits, {w}x{d})");
    let t = Instant::now();
    let mut n = 0usize;
    for _ in 0..rounds {
        n += room.iter().count();
    }
    eprintln!(
        "room.iter(): {:.1} us per call ({n} cells)",
        t.elapsed().as_nanos() as f64 / rounds as f64 / 1000.0
    );
    let t = Instant::now();
    let mut faces = 0usize;
    for _ in 0..200 {
        faces += super::faces::all_faces(world).len();
    }
    eprintln!(
        "all_faces: {:.3} ms ({faces} faces over 200 runs)",
        t.elapsed().as_secs_f64() * 1000.0 / 200.0
    );
    let t = Instant::now();
    for _ in 0..200 {
        std::hint::black_box(world.clone());
    }
    eprintln!(
        "world clone: {:.3} ms",
        t.elapsed().as_secs_f64() * 1000.0 / 200.0
    );
}
