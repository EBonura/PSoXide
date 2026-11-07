//! Plane runs shared across sectors and leaves.
//!
//! A free plane or a mesh proxy touches several sectors, and a tree room
//! splits each sector into up to sixteen leaves. The cook used to write the
//! records of every leaf's list as a run of its own, so a plane was copied
//! once per leaf it touched (the Outpost's 40 collidable boxes: 4,639 copies
//! of 200 records) and every point ray walked the whole copy table. Leaves
//! whose lists are identical now point at one run.
//!
//! The first group of tests pins the count. The second proves the runtime
//! cannot tell: [`unshare`] writes the table the way the cook used to (one
//! private run per extension record), and the same rays, bodies and walks are
//! traced through both.

use std::collections::HashSet;

use psx_engine::{CollisionTrace, CollisionTraceProvider, CollisionTraceQuery, RoomPoint};
use psx_grid::collision::{GridCollision, GridTraceProvider};
use psx_grid::write::{aligned_copy, RoomChunk};
use psx_grid::{CollisionView, LevelView, RoomView, COLPLANE_PRIMARY};

use super::tests::cook;
use crate::grid::leaf::LeafKey;
use crate::grid::mesh::{ArchSpec, GridMesh, ImportProxy, ImportedMesh, MeshAsset, MeshKind};
use crate::grid::plane::{GridPlane, PlaneCollision};
use crate::grid::room::GridRoom;
use crate::grid::sector::GridSector;
use crate::grid::surface::GridSurface;
use crate::grid::units::{GridScale, SECTOR_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

fn leak(bytes: &[u8]) -> &'static [u8] {
    let words: &'static mut [u32] = aligned_copy(bytes).leak();
    // SAFETY: the words live for the program and cover `bytes.len()` bytes.
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes.len()) }
}

const WIDTH: usize = 12;
const DEPTH: usize = 8;

fn floor() -> GridSector {
    GridSector::with_surfaces(Some(GridSurface::flat(0, Some(rid(7)))), None)
}

/// A flat hall of whole sectors.
fn dense_hall() -> GridWorld {
    let mut room = GridRoom::new("hall", WIDTH, DEPTH);
    for z in 0..DEPTH {
        for x in 0..WIDTH {
            room.sector_or_insert(x, z)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, Some(rid(7)))));
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// The same hall as a tree room: half sectors and quarter sectors in a
/// checkerboard (up to sixteen leaves in a sector, as in the Outpost).
fn tree_hall() -> GridWorld {
    let mut room = GridRoom::new("tree", WIDTH, DEPTH);
    for sz in 0..DEPTH as i32 {
        for sx in 0..WIDTH as i32 {
            let (level, step) = if (sx + sz) % 2 == 0 { (3, 2) } else { (4, 1) };
            for j in (0..4).step_by(step) {
                for i in (0..4).step_by(step) {
                    let key = LeafKey::new(level, sx * 4 + i, sz * 4 + j).unwrap();
                    room.insert_leaf(key, floor()).unwrap();
                }
            }
        }
    }
    assert!(!room.leaves.is_sectors_only());
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

const CUBE: &str = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0 0 1\nv 1 0 1\nv 1 1 1\nv 0 1 1\n\
                    f 5 6 7 8\nf 2 1 4 3\nf 1 5 8 4\nf 6 2 3 7\nf 8 7 3 4\nf 1 2 6 5\n";

/// The cube asset (a sector on a side at scale 1) as asset index 0.
fn with_cube(world: &mut GridWorld) {
    let cube = ImportedMesh::from_obj(CUBE, 1024.0, "box.obj", ImportProxy::BoxTop).unwrap();
    world
        .mesh_assets
        .push(MeshAsset::new("Block", MeshKind::Imported(cube)));
}

/// A standable box centred on `centre` (sector coordinates, may be
/// fractional as hundredths) with the given plan size and height in grid
/// units.
fn place_box(world: &mut GridWorld, id: u16, centre: [i32; 2], size: [i32; 3]) {
    let mut mesh = GridMesh::new(
        id,
        0,
        [
            centre[0] * SECTOR_UNITS / 100,
            0,
            centre[1] * SECTOR_UNITS / 100,
        ],
    );
    // The asset is a sector on a side; Q8 scale 256 is 1.0.
    mesh.scale = size.map(|v| (v / 4) as u16);
    mesh.set_material(0, Some(rid(8)));
    world.rooms[0].meshes.push(mesh);
}

/// A box over exactly the 2 by 2 sectors around the corner `(cx, cz)`.
fn corner_box(world: &mut GridWorld, id: u16, corner: [i32; 2]) {
    place_box(
        world,
        id,
        [corner[0] * 100, corner[1] * 100],
        [1920, 512, 1920],
    );
}

/// Six boxes, each over its own 2 by 2 sectors.
fn six_boxes(mut world: GridWorld) -> GridWorld {
    with_cube(&mut world);
    for (id, corner) in [[2, 2], [5, 2], [9, 2], [2, 6], [6, 6], [10, 6]]
        .into_iter()
        .enumerate()
    {
        corner_box(&mut world, id as u16 + 1, corner);
    }
    world
}

/// One long deck over the 5 by 3 sectors x 4 to 8, z 3 to 5.
fn deck(mut world: GridWorld) -> GridWorld {
    with_cube(&mut world);
    place_box(&mut world, 1, [650, 450], [4900, 1024, 2900]);
    world
}

fn unique_ids(planes: &[psx_grid::ColPlane]) -> usize {
    planes.iter().map(|p| p.id).collect::<HashSet<_>>().len()
}

/// Everything a tree room says about one sector's planes: every leaf of a
/// sector sees the same list.
fn runs_of_sector(room: &RoomView<'_>, x: usize, z: usize) -> Vec<(u16, u8)> {
    let mut runs = Vec::new();
    let s = room.col.index[z * room.col.width() + x];
    if s & psx_grid::INDEX_SUB == 0 {
        let sector = &room.col.sectors[usize::from(s)];
        if let Some(ext) = room.col.ext(sector) {
            runs.push((ext.plane_first, ext.plane_count));
        }
    } else {
        let block = usize::from(s & psx_grid::INDEX_ID) * psx_grid::SUBBLOCK_ENTRIES;
        for k in 0..psx_grid::SUBBLOCK_ENTRIES {
            let leaf = room.col.blocks[block + k];
            if leaf == psx_grid::NO_LEAF {
                continue;
            }
            let sector = &room.col.sectors[usize::from(leaf)];
            if let Some(ext) = room.col.ext(sector) {
                runs.push((ext.plane_first, ext.plane_count));
            }
        }
    }
    runs
}

#[test]
fn a_tree_room_writes_the_records_of_its_sectors_not_of_its_leaves() {
    let tree = cook(&six_boxes(tree_hall())).unwrap();
    let dense = cook(&six_boxes(dense_hall())).unwrap();
    let tree = RoomView::parse(leak(&tree.chunks[0])).unwrap();
    let dense = RoomView::parse(leak(&dense.chunks[0])).unwrap();
    // Four walls and a top per box.
    assert_eq!(unique_ids(tree.col.col_planes), 30);
    // A box over 2 by 2 sectors is four different lists (top and the two
    // walls of its corner, three records each): 12 a box.
    assert_eq!(dense.col.col_planes.len(), 72);
    // The leaves of a sector all share the sector's run: the tree room costs
    // what the dense one does, with up to sixteen leaves to a sector.
    assert_eq!(
        tree.col.col_planes.len(),
        72,
        "{} records for 30 planes",
        tree.col.col_planes.len()
    );
    assert_eq!(
        tree.col
            .col_planes
            .iter()
            .filter(|p| p.flags & COLPLANE_PRIMARY != 0)
            .count(),
        30,
        "every plane keeps exactly one point-ray record"
    );
    // The leaves of one sector point at one run.
    for (x, z) in [(1, 1), (2, 2), (5, 1), (4, 2), (1, 5), (2, 6)] {
        let runs = runs_of_sector(&tree, x, z);
        assert!(
            !runs.is_empty(),
            "sector {x},{z}: index {:?}",
            tree.col.index[z * tree.col.width() + x]
        );
        assert!(
            runs.iter().all(|r| *r == runs[0] && r.1 == 3),
            "sector {x},{z}: {runs:?}"
        );
    }
}

#[test]
fn the_cook_report_counts_plane_records_per_room() {
    let cooked = cook(&six_boxes(tree_hall())).unwrap();
    let room = &cooked.report.rooms[0];
    assert_eq!((room.plane_records, room.planes), (72, 30));
    // What the lists name together is what every leaf used to be given.
    assert!(room.plane_listed >= 10 * room.plane_records, "{room:?}");
    let text = cooked.report.summary();
    assert!(text.contains("72 plane records for 30 planes"), "{text}");
    // A room without planes says nothing about them.
    let bare = cook(&dense_hall()).unwrap().report.summary();
    assert!(!bare.contains("plane records"), "{bare}");
}

#[test]
fn sectors_with_the_same_list_share_a_run_in_dense_and_tree_rooms() {
    // A 5 by 3 deck: the three middle sectors list only the top, the three
    // along each long edge the top and that edge's wall, the two ends the top
    // and their wall, the four corners three. Distinct lists: 1 + 2 + 2 + 2 +
    // 2 + 4 * 3 = 21 records (31 without sharing).
    for (name, world) in [("dense", deck(dense_hall())), ("tree", deck(tree_hall()))] {
        let cooked = cook(&world).unwrap();
        let room = RoomView::parse(leak(&cooked.chunks[0])).unwrap();
        assert_eq!(unique_ids(room.col.col_planes), 5, "{name}");
        assert_eq!(room.col.col_planes.len(), 21, "{name}");
    }
}

#[test]
fn overlapping_boxes_cost_only_their_distinct_lists() {
    // Two boxes that overlap on one column of sectors: the lists are those of
    // each box alone and of both together, never one run per leaf.
    let mut world = tree_hall();
    with_cube(&mut world);
    corner_box(&mut world, 1, [3, 3]);
    corner_box(&mut world, 2, [4, 3]);
    let cooked = cook(&world).unwrap();
    let room = RoomView::parse(leak(&cooked.chunks[0])).unwrap();
    let planes = room.col.col_planes;
    assert_eq!(unique_ids(planes), 10);
    let mut dense = dense_hall();
    with_cube(&mut dense);
    corner_box(&mut dense, 1, [3, 3]);
    corner_box(&mut dense, 2, [4, 3]);
    let dense = cook(&dense).unwrap();
    let dense = RoomView::parse(leak(&dense.chunks[0])).unwrap();
    assert_eq!(planes.len(), dense.col.col_planes.len());
}

/// What the cook wrote before runs were shared: every extension record owns a
/// private copy of its list, the first copy of each plane is the one point
/// rays test.
fn unshare(chunk: &RoomChunk) -> RoomChunk {
    let mut out = chunk.clone();
    out.col_planes.clear();
    let mut seen = HashSet::new();
    for k in 0..out.exts.len() {
        let first = usize::from(chunk.exts[k].plane_first);
        let count = usize::from(chunk.exts[k].plane_count);
        out.exts[k].plane_first = out.col_planes.len() as u16;
        for plane in &chunk.col_planes[first..first + count] {
            let mut copy = *plane;
            copy.flags &= !COLPLANE_PRIMARY;
            if seen.insert(copy.id) {
                copy.flags |= COLPLANE_PRIMARY;
            }
            out.col_planes.push(copy);
        }
    }
    out
}

fn collision_of(core: &[u8], chunks: &[Vec<u8>]) -> GridCollision<'static> {
    let level = LevelView::parse(leak(core)).unwrap();
    let rooms: Vec<Option<CollisionView<'static>>> = chunks
        .iter()
        .map(|c| Some(CollisionView::parse(leak(c)).unwrap()))
        .collect();
    let rooms: &'static [Option<CollisionView<'static>>] = rooms.leak();
    psx_grid::ResidentRooms::from_collisions(level, rooms)
        .expect("a collision world")
        .leak_collision()
}

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u32) as i32
    }
}

fn trace(world: &GridCollision<'static>, query: CollisionTraceQuery) -> Option<CollisionTrace> {
    let mut out = CollisionTrace::default();
    GridTraceProvider::new(world)
        .trace_into(query, &mut out)
        .then_some(out)
}

/// How many rays each level is sampled with, per room.
const RAYS_PER_ROOM: usize = 1500;

/// Cook `world`, write the table both ways, and trace the same sampled rays
/// (points and bodies) and sight lines through both. Returns how many of the
/// point rays hit something and how many records each table holds.
fn assert_same_collision(world: &GridWorld, label: &str) -> (usize, usize, usize) {
    let cooked = cook(world).unwrap_or_else(|e| panic!("{label}: {e}"));
    let shared: Vec<Vec<u8>> = cooked.tables.iter().map(RoomChunk::to_bytes).collect();
    let private: Vec<Vec<u8>> = cooked
        .tables
        .iter()
        .map(|t| unshare(t).to_bytes())
        .collect();
    // The tables with sharing really are the ones in the cooked chunks.
    assert_eq!(shared, cooked.chunks, "{label}");
    let a = collision_of(&cooked.core, &shared);
    let b = collision_of(&cooked.core, &private);
    let mut hits = 0;
    let mut checked = 0;
    let mut rng = Rng(0x9e37_79b9);
    for (index, rec) in cooked.level.rooms.iter().enumerate() {
        if cooked.tables[index].col_planes.is_empty() {
            continue;
        }
        let s = i32::from(cooked.level.sector_units);
        let (w, d) = (i32::from(rec.width) * s, i32::from(rec.depth) * s);
        let (y0, y1) = (i32::from(rec.min_y) - 40, i32::from(rec.max_y) + 160);
        for _ in 0..RAYS_PER_ROOM {
            let start = [
                rec.origin[0] + rng.range(0, w - 1),
                rec.origin[1] + rng.range(y0, y1),
                rec.origin[2] + rng.range(0, d - 1),
            ];
            let end = [
                start[0] + rng.range(-2 * s, 2 * s),
                start[1] + rng.range(-s, s),
                start[2] + rng.range(-2 * s, 2 * s),
            ];
            let (from, to) = (RoomPoint::from_array(start), RoomPoint::from_array(end));
            for query in [
                CollisionTraceQuery::point(from, to),
                CollisionTraceQuery::body(from, to, 12, 48),
                CollisionTraceQuery::body(from, to, 24, 96),
            ] {
                let (ta, tb) = (trace(&a, query), trace(&b, query));
                assert_eq!(ta, tb, "{label}: room {index}, {query:?}");
                checked += 1;
                if matches!(query.shape, psx_engine::CollisionTraceShape::Point)
                    && ta.is_some_and(CollisionTrace::hit)
                {
                    hits += 1;
                }
            }
            assert_eq!(
                a.line_of_sight(start, end),
                b.line_of_sight(start, end),
                "{label}: room {index}, sight {start:?} -> {end:?}"
            );
        }
    }
    let records = |chunks: &[Vec<u8>]| {
        chunks
            .iter()
            .map(|c| RoomView::parse(leak(c)).unwrap().col.col_planes.len())
            .sum()
    };
    let _ = checked;
    (hits, records(&shared), records(&private))
}

/// Both layouts of a busy hall: overlapping boxes, a free bridge, an arch.
fn busy(mut world: GridWorld) -> GridWorld {
    with_cube(&mut world);
    world
        .mesh_assets
        .push(MeshAsset::new("arch", MeshKind::Arch(ArchSpec::default())));
    corner_box(&mut world, 1, [3, 3]);
    corner_box(&mut world, 2, [4, 3]);
    place_box(&mut world, 3, [850, 550], [2800, 640, 2000]);
    place_box(&mut world, 5, [150, 650], [900, 1500, 1800]);
    let mut arch = GridMesh::new(4, 1, [6 * SECTOR_UNITS, 0, 2 * SECTOR_UNITS]);
    arch.set_material(0, Some(rid(8)));
    arch.set_material(1, Some(rid(9)));
    world.rooms[0].meshes.push(arch);
    let mut bridge = GridPlane::new([9 * SECTOR_UNITS, 1024, 2 * SECTOR_UNITS], [4096, 1024]);
    bridge.rotation = [0, 90, 0];
    bridge.collision = PlaneCollision::Walkable;
    bridge.material = Some(rid(8));
    world.rooms[0].planes.push(bridge);
    world
}

#[test]
fn shared_runs_trace_exactly_like_private_copies_in_a_dense_room() {
    let (hits, shared, private) = assert_same_collision(&busy(dense_hall()), "dense busy hall");
    assert!(hits > 50, "the rays must reach the planes ({hits})");
    assert!(shared <= private, "{shared} records against {private}");
}

#[test]
fn shared_runs_trace_exactly_like_private_copies_in_a_tree_room() {
    let (hits, shared, private) = assert_same_collision(&busy(tree_hall()), "tree busy hall");
    assert!(hits > 50, "the rays must reach the planes ({hits})");
    assert!(
        shared * 8 < private,
        "{shared} records against {private} private copies"
    );
}

/// The reference levels (the tracked ones, and the local ones when present).
/// Most hold no free planes, which makes the check the cheap statement that
/// nothing moved; the ones with planes and meshes are the real test.
const LEVELS: [&str; 16] = [
    "grid-cathedral",
    "grid-level008",
    "grid-dungeon",
    "grid-cave",
    "grid-sewers",
    "grid-west",
    "grid-gate",
    "grid-tower",
    "grid-corridor",
    "grid-starter",
    "grid-planes",
    "grid-outpost",
    "grid-outpost-v2",
    "grid-outpost-x3",
    "grid-ruins",
    "dp-world",
];

#[test]
fn the_reference_levels_trace_the_same_with_shared_runs() {
    let mut with_planes = 0;
    for name in LEVELS {
        // The planes demo is a committed fixture; the rest live in the
        // projects directory.
        let path = if name == "grid-planes" {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/grid-planes/project.ron")
        } else {
            crate::projects_dir().join(name).join("project.ron")
        };
        let Ok(project) = crate::ProjectDocument::load_from_path(path) else {
            eprintln!("plane runs: {name} is not on this machine, skipped");
            continue;
        };
        let Some(world) = project.active_scene().world.grid() else {
            continue;
        };
        let (hits, shared, private) = assert_same_collision(world, name);
        if private > 0 {
            with_planes += 1;
        }
        eprintln!(
            "plane runs: {name}: {private} records before, {shared} now, {hits} point rays hit"
        );
    }
    eprintln!("plane runs: {with_planes} reference levels hold plane records");
    // The committed planes fixture is always there, so this test always has a
    // level with planes to compare, whatever else is on the machine.
    assert!(with_planes >= 1, "the grid-planes fixture holds planes");
}
