//! Faces across leaf sizes (T9): the leaf path agrees with the sector path
//! where the leaves are sectors, and a subdivided room is watertight.

use std::collections::{BTreeMap, BTreeSet};

use super::faces::{all_faces, GridFace};
use super::leaf::LeafKey;
use super::leaf_faces;
use super::overrides::WallMode;
use super::room::GridRoom;
use super::sector::GridSector;
use super::surface::GridSurface;
use super::tree::{normalize, subdivide, subdivide_to};
use super::units::GridScale;
use super::world::GridWorld;

const PROJECTS: [&str; 7] = [
    "grid-cathedral",
    "grid-level008",
    "grid-dungeon",
    "grid-cave",
    "grid-sewers",
    "grid-west",
    "grid-starter",
];

fn load(name: &str) -> crate::ProjectDocument {
    let path = crate::projects_dir().join(name).join("project.ron");
    crate::ProjectDocument::load_from_path(path).expect("project loads")
}

struct Rng(u32);

impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) % n.max(1)
    }
}

/// Every face as a comparable tuple, ignoring the fields the leaf path adds.
type Shape = (usize, [i32; 2], String, Vec<[i32; 3]>, Vec<[i32; 3]>);

fn shape(f: &GridFace) -> Shape {
    (
        f.room,
        f.cell,
        format!("{:?}", f.kind),
        f.points.clone(),
        f.grid.clone(),
    )
}

#[test]
fn the_leaf_path_makes_the_faces_of_the_sector_path_for_whole_sectors() {
    // Run the leaf path over every cell of every reference room. The only
    // differences allowed are the ones the leaf path states: banded wall
    // stitching.
    let mut compared = 0;
    for name in PROJECTS {
        let project = load(name);
        let world = project.active_scene().world.grid().expect("a grid project");
        for (index, room) in world.rooms.iter().enumerate() {
            if !room.band_sets.is_empty() {
                continue;
            }
            for [x, z] in room.face_cells() {
                let mut sector_path = Vec::new();
                super::faces::cell_faces(world, index, x, z, &mut sector_path);
                let mut leaf_path = Vec::new();
                leaf_faces::cell_faces(world, &(), index, x, z, &mut leaf_path);
                let a: Vec<_> = sector_path.iter().map(shape).collect();
                let b: Vec<_> = leaf_path.iter().map(shape).collect();
                assert_eq!(
                    a.len(),
                    b.len(),
                    "{name} room {index} cell {x},{z}: {} faces vs {}",
                    a.len(),
                    b.len()
                );
                let sorted = |mut v: Vec<_>| {
                    v.sort_by(|p: &Shape, q| (&p.2, &p.4).cmp(&(&q.2, &q.4)));
                    v
                };
                assert_eq!(sorted(a), sorted(b), "{name} room {index} cell {x},{z}");
                compared += 1;
            }
        }
    }
    assert!(compared > 500, "{compared} cells compared");
}

/// A derived `w` by `d` room with cliffs: floors at multiples of 256 up to
/// 1024 in 4 by 4 patches, a flat ceiling at 2560.
fn cliff_room(rng: &mut Rng, w: usize, d: usize) -> GridRoom {
    let mut room = GridRoom::new_derived("r", w, d);
    let mut patch = vec![0i32; (w / 2 + 1) * (d / 2 + 1)];
    for p in &mut patch {
        *p = 256 * rng.below(4) as i32;
    }
    for z in 0..d {
        for x in 0..w {
            let h = patch[(z / 2) * (w / 2 + 1) + x / 2];
            let floor = GridSurface::flat(h, None);
            let ceiling = GridSurface::flat(2560, None);
            room.set_sector(
                x,
                z,
                Some(GridSector::with_surfaces(Some(floor), Some(ceiling))),
            );
        }
    }
    room
}

fn strictly_inside(a: [i32; 3], b: [i32; 3], v: [i32; 3]) -> bool {
    let d = [0, 1, 2].map(|i| i64::from(b[i] - a[i]));
    let e = [0, 1, 2].map(|i| i64::from(v[i] - a[i]));
    let cross = [
        d[1] * e[2] - d[2] * e[1],
        d[2] * e[0] - d[0] * e[2],
        d[0] * e[1] - d[1] * e[0],
    ];
    if cross != [0, 0, 0] {
        return false;
    }
    let dot = d[0] * e[0] + d[1] * e[1] + d[2] * e[2];
    let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    dot > 0 && dot < len2
}

/// The T-junctions on floor and ceiling edges: a vertex of any face that
/// lies strictly inside an edge of a floor or ceiling polygon. (Walls meet
/// at a floor step in a vertical edge of unequal length in every level made
/// before the quadtree; that is the cook's repair, design 9.3b, not this.)
fn surface_tee_junctions(world: &GridWorld) -> Vec<([i32; 3], [i32; 3], [i32; 3])> {
    let faces = all_faces(world);
    let vertices: BTreeSet<[i32; 3]> = faces.iter().flat_map(|f| f.grid.iter().copied()).collect();
    let mut found = Vec::new();
    for f in faces.iter().filter(|f| {
        matches!(
            f.kind,
            super::faces::GridFaceKind::Floor { .. } | super::faces::GridFaceKind::Ceiling { .. }
        )
    }) {
        let n = f.grid.len();
        for i in 0..n {
            let (a, b) = (f.grid[i], f.grid[(i + 1) % n]);
            for v in &vertices {
                // A bow tie (two profiles that cross inside a segment) puts a
                // vertex off the click lattice; it is cut the way a sector
                // room cuts it and not what stitching is for.
                if v[0] % 256 != 0 || v[2] % 256 != 0 {
                    continue;
                }
                if strictly_inside(a, b, *v) {
                    found.push((a, b, *v));
                }
            }
        }
    }
    found
}

/// Every edge of a floor or ceiling is shared by exactly two faces, or lies
/// on the border of the room's footprint where only the wall beside it
/// closes it. Returns the edges used by one face with no wall along them.
fn surface_edges_without_neighbour(world: &GridWorld) -> Vec<([i32; 3], [i32; 3])> {
    let faces = all_faces(world);
    let mut count: BTreeMap<([i32; 3], [i32; 3]), usize> = BTreeMap::new();
    for f in &faces {
        let n = f.grid.len();
        for i in 0..n {
            let (a, b) = (f.grid[i], f.grid[(i + 1) % n]);
            if a != b {
                *count.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
    }
    let mut lonely = Vec::new();
    for f in faces.iter().filter(|f| {
        matches!(
            f.kind,
            super::faces::GridFaceKind::Floor { .. } | super::faces::GridFaceKind::Ceiling { .. }
        )
    }) {
        let n = f.grid.len();
        for i in 0..n {
            let (a, b) = (f.grid[i], f.grid[(i + 1) % n]);
            if a != b && count[&(a.min(b), a.max(b))] < 2 {
                lonely.push((a, b));
            }
        }
    }
    lonely
}

fn world_of(room: GridRoom) -> GridWorld {
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

#[test]
fn a_room_of_whole_sectors_is_watertight_to_begin_with() {
    let mut rng = Rng(5);
    let world = world_of(cliff_room(&mut rng, 8, 8));
    assert!(surface_tee_junctions(&world).is_empty());
    assert!(surface_edges_without_neighbour(&world).is_empty());
}

#[test]
fn subdivided_rooms_are_watertight_with_no_t_junction() {
    let mut judged = 0;
    for seed in 0..40 {
        let mut rng = Rng(seed + 40);
        let mut room = cliff_room(&mut rng, 8, 8);
        // Subdivide random places to random depths, in a few waves.
        for _ in 0..(3 + rng.below(6)) {
            let click = [rng.below(32) as i32, rng.below(32) as i32];
            let level = 3 + rng.below(2) as u8;
            let _ = subdivide_to(&mut room, click, level);
        }
        // Lift a few fine corners the way a height drag does.
        for _ in 0..4 {
            let keys: Vec<LeafKey> = room.leaves.keys().filter(|k| k.level > 2).collect();
            if keys.is_empty() {
                break;
            }
            let key = keys[rng.below(keys.len() as u32) as usize];
            if let Some(f) = room.leaf_mut(key).and_then(|s| s.ground_floor_mut()) {
                f.heights[rng.below(4) as usize] += 256;
            }
            normalize(&mut room);
        }
        assert!(room.leaves.keys().any(|k| k.level > 2), "seed {seed}");
        let world = world_of(room);
        // A bow tie (two floor profiles that cross inside a segment) is cut
        // at the crossing, off the click lattice, the way a sector room cuts
        // it; its edges are split on both sides. Judge the rest.
        if all_faces(&world)
            .iter()
            .flat_map(|f| f.grid.iter())
            .any(|g| g[0] % 256 != 0 || g[2] % 256 != 0)
        {
            continue;
        }
        judged += 1;
        let tee = surface_tee_junctions(&world);
        assert!(tee.is_empty(), "seed {seed}: T-junctions {tee:?}");
        let lonely = surface_edges_without_neighbour(&world);
        assert!(lonely.is_empty(), "seed {seed}: unshared edges {lonely:?}");
    }
    assert!(judged >= 10, "only {judged} rooms were free of bow ties");
}

#[test]
fn big_leaves_are_cut_at_sector_borders_and_stay_watertight() {
    let mut room = GridRoom::new_derived("r", 8, 8);
    for z in 0..8 {
        for x in 0..8 {
            room.set_sector(
                x,
                z,
                Some(GridSector::with_surfaces(
                    Some(GridSurface::flat(0, None)),
                    Some(GridSurface::flat(2048, None)),
                )),
            );
        }
    }
    super::tree::merge_all(&mut room, None, 0);
    assert_eq!(room.leaf_count(), 4);
    // Subdivide a corner to the finest level so big leaves meet fine ones.
    let _ = subdivide_to(&mut room, [0, 0], 4);
    let world = world_of(room);
    let faces = all_faces(&world);
    // Every face lies in the sector it is listed under.
    for f in &faces {
        let [cx, cz] = f.cell;
        for g in &f.grid {
            assert!(
                (g[0] >= cx * 1024 && g[0] <= cx * 1024 + 1024)
                    && (g[2] >= cz * 1024 && g[2] <= cz * 1024 + 1024),
                "{:?} in {:?} {:?}",
                f.kind,
                f.cell,
                g
            );
        }
    }
    assert!(surface_tee_junctions(&world).is_empty());
    assert!(surface_edges_without_neighbour(&world).is_empty());
}

#[test]
fn a_derived_wall_follows_the_height_difference_between_leaf_sizes() {
    // A raised sector next to one split in four: the riser between them is
    // two segments, one per fine leaf, each at the fine leaf's own height.
    let mut room = GridRoom::new_derived("r", 2, 1);
    for x in 0..2 {
        let h = if x == 0 { 512 } else { 0 };
        room.set_sector(
            x,
            0,
            Some(GridSector::with_surfaces(
                Some(GridSurface::flat(h, None)),
                Some(GridSurface::flat(2560, None)),
            )),
        );
    }
    subdivide(&mut room, LeafKey::sector([1, 0])).unwrap();
    // Lift the north half of the fine side by 128 more than the low sector.
    let north = LeafKey::new(3, 4, 0).unwrap();
    room.leaf_mut(north)
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .heights = [128; 4];
    let world = world_of(room);
    let faces = all_faces(&world);
    let risers: Vec<&GridFace> = faces
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                super::faces::GridFaceKind::Derived {
                    kind: super::derive::DerivedKind::Riser,
                    ..
                }
            ) && f.grid.iter().all(|g| g[0] == 1024)
        })
        .collect();
    // The coarse sector stands at 512 beside fine floors at 128 and 0: two
    // risers, 512 - 128 and 512 - 0 tall, each half a sector wide.
    assert_eq!(risers.len(), 2, "{risers:?}");
    let mut heights: Vec<i32> = risers
        .iter()
        .map(|f| {
            let ys = f.grid.iter().map(|g| g[1]);
            ys.clone().max().unwrap() - ys.min().unwrap()
        })
        .collect();
    heights.sort_unstable();
    assert_eq!(heights, vec![384, 512]);
    assert!(surface_tee_junctions(&world).is_empty());
    assert!(world.rooms[0].wall_mode == WallMode::Derived);
}
