//! Tree rooms (`CHUNK_TREE`): leaf lookup, walls, rays and traces over
//! leaves of every size, against a brute-force leaf list and against the
//! same rooms in the dense layout.

extern crate std;

use super::*;
use crate::height::{height_at, weight_recip};
use crate::write::TreeTables;

/// Clicks (quarter sectors) in a sector.
const C: i32 = 4;
/// Edge of a click, runtime units.
const Q: i32 = S / 4;

/// A leaf of a tree room, positioned in room-local clicks.
#[derive(Clone)]
pub(super) struct TLeaf {
    pub x: i32,
    pub z: i32,
    pub code: u16,
    pub sector: Sector,
    pub walls: Vec<ColWall>,
    pub split: Option<SplitB>,
}

impl TLeaf {
    pub(super) fn new(x: i32, z: i32, code: u16, floor: [i16; 4]) -> Self {
        Self {
            x,
            z,
            code,
            sector: Sector {
                floor,
                ceil: [0; 4],
                flags: FLAT,
                ext: NO_EXT,
            },
            walls: Vec::new(),
            split: None,
        }
    }

    /// Edge in clicks.
    pub(super) fn clicks(&self) -> i32 {
        match self.code {
            leaf_code::X2 => 8,
            leaf_code::X4 => 16,
            leaf_code::HALF => 2,
            leaf_code::QUARTER => 1,
            _ => 4,
        }
    }

    fn edge(&self) -> i32 {
        leaf_edge(S, self.code)
    }
}

pub(super) struct TreeRoom {
    pub origin: [i32; 3],
    pub w: u8,
    pub d: u8,
    /// World origin of the room modulo four sectors.
    pub om: (i32, i32),
    pub leaves: Vec<TLeaf>,
}

impl TreeRoom {
    pub(super) fn new(origin: [i32; 3], w: u8, d: u8, om: (i32, i32)) -> Self {
        Self {
            origin,
            w,
            d,
            om,
            leaves: Vec::new(),
        }
    }

    /// Index of the leaf covering click `(x, z)`, brute force.
    pub(super) fn leaf_index_at(&self, x: i32, z: i32) -> Option<usize> {
        self.leaves
            .iter()
            .position(|l| x >= l.x && x < l.x + l.clicks() && z >= l.z && z < l.z + l.clicks())
    }

    /// The leaf covering click `(x, z)`, brute force.
    pub(super) fn leaf_at(&self, x: i32, z: i32) -> Option<&TLeaf> {
        self.leaf_index_at(x, z).map(|i| &self.leaves[i])
    }

    pub(super) fn chunk(&self) -> RoomChunk {
        let (w, d) = (usize::from(self.w), usize::from(self.d));
        let mut sectors = Vec::new();
        let mut exts: Vec<SectorExt> = Vec::new();
        let mut walls = Vec::new();
        let mut splitbs = Vec::new();
        for leaf in &self.leaves {
            let mut rec = leaf.sector;
            rec.flags |= leaf.code << SECTOR_LEAF_SHIFT;
            if !leaf.walls.is_empty() || leaf.split.is_some() {
                let mut ext = SectorExt {
                    wall_first: walls.len() as u16,
                    wall_count: leaf.walls.len() as u8,
                    plane_count: 0,
                    plane_first: 0,
                    splitb: NO_EXT,
                };
                walls.extend(leaf.walls.iter().copied());
                if let Some(b) = leaf.split {
                    ext.splitb = splitbs.len() as u16;
                    splitbs.push(b);
                    rec.flags |= SECTOR_SPLIT_LEVEL;
                }
                rec.ext = exts.len() as u16;
                exts.push(ext);
            }
            sectors.push(rec);
        }
        crate::collision::summarize_leaves(&mut sectors);
        let mut index = Vec::new();
        let mut blocks: Vec<u16> = Vec::new();
        for cz in 0..d as i32 {
            for cx in 0..w as i32 {
                let mut ids = [NO_LEAF; 16];
                for k in 0..16 {
                    let (x, z) = (cx * C + (k & 3), cz * C + (k >> 2));
                    if let Some(i) = self.leaf_index_at(x, z) {
                        ids[k as usize] = i as u16;
                    }
                }
                let first = ids[0];
                let whole = ids.iter().all(|&i| i == first)
                    && (first == NO_LEAF || self.leaves[usize::from(first)].clicks() >= C);
                if whole {
                    index.push(first);
                } else {
                    index.push(INDEX_SUB | (blocks.len() / 16) as u16);
                    blocks.extend_from_slice(&ids);
                }
            }
        }
        RoomChunk {
            width: self.w,
            depth: self.d,
            sectors,
            tree: Some(TreeTables {
                origin_mod: (self.om.0 | self.om.1 << 2) as u8,
                index,
                blocks,
            }),
            exts,
            walls,
            splitbs,
            ..RoomChunk::default()
        }
    }
}

/// Build a level out of already made chunks (`tree[i]` says whether room `i`
/// is a tree room).
pub(super) fn build_chunks(rooms: &[(Room, RoomChunk, bool)]) -> Built {
    let core = LevelCore {
        sector_units: S as u16,
        rooms: rooms
            .iter()
            .map(|(r, _, tree)| RoomRecord {
                origin: r.origin,
                width: r.w,
                depth: r.d,
                min_y: -200,
                max_y: 400,
                flags: if *tree { ROOM_TREE } else { 0 },
                ..RoomRecord::default()
            })
            .collect(),
        ..LevelCore::default()
    };
    let bytes = core.to_bytes();
    let chunks = rooms
        .iter()
        .map(|(_, c, _)| {
            let b = c.to_bytes();
            (aligned_copy(&b), b.len())
        })
        .collect();
    Built {
        level: aligned_copy(&bytes),
        level_len: bytes.len(),
        rooms: chunks,
    }
}

fn tree_world(rooms: &[TreeRoom]) -> Built {
    let items: Vec<(Room, RoomChunk, bool)> = rooms
        .iter()
        .map(|r| (Room::flat(r.origin, r.w, r.d, 0), r.chunk(), true))
        .collect();
    build_chunks(&items)
}

/// The dense room `room` re-encoded as a tree room whose leaves are all
/// whole sectors.
fn forced_tree(room: &Room) -> (Room, RoomChunk, bool) {
    let mut chunk = room.chunk(false);
    crate::collision::summarize_leaves(&mut chunk.sectors);
    chunk.tree = Some(TreeTables {
        origin_mod: 0,
        index: (0..chunk.sectors.len() as u16).collect(),
        blocks: Vec::new(),
    });
    (room.clone(), chunk, true)
}

fn dense(room: &Room) -> (Room, RoomChunk, bool) {
    (room.clone(), room.chunk(true), false)
}

/// Reference height of a leaf at a room-local point.
fn reference_floor(leaf: &TLeaf, x: i32, z: i32) -> Option<i32> {
    if leaf.sector.flags & SECTOR_HAS_FLOOR == 0 {
        return None;
    }
    let size = leaf.edge();
    let (lx, lz) = (x - leaf.x * Q, z - leaf.z * Q);
    let nesw = leaf.sector.flags & SECTOR_FLOOR_NESW != 0;
    let corners = leaf.sector.floor;
    let second = !crate::height::in_first_triangle(nesw, lx as u32, lz as u32, size as u32);
    let corners = match (second, leaf.split) {
        (true, Some(b)) => b.floor_b,
        _ => corners,
    };
    Some(height_at(
        corners,
        nesw,
        lx as u32,
        lz as u32,
        size as u32,
        weight_recip(size as u32),
    ))
}

/// A random tree over a `w` by `d` sector room: each 4 by 4 sector block is
/// split at random into leaves of every size, with random corner heights.
fn random_room(seed: u32, w: u8, d: u8, om: (i32, i32)) -> TreeRoom {
    let mut rng = Lcg(seed);
    let mut room = TreeRoom::new([0, 0, 0], w, d, om);
    // World-aligned blocks of 16 clicks, clipped to the room.
    let (wx, wz) = (i32::from(w), i32::from(d));
    let mut stack: Vec<(i32, i32, u16)> = Vec::new();
    let (ox, oz) = (om.0, om.1);
    // World block origins, in sectors, that overlap the room.
    let bx0 = -ox;
    let bz0 = -oz;
    let mut bz = bz0;
    while bz < wz {
        let mut bx = bx0;
        while bx < wx {
            stack.push((bx * C, bz * C, leaf_code::X4));
            bx += 4;
        }
        bz += 4;
    }
    while let Some((x, z, code)) = stack.pop() {
        let n = match code {
            leaf_code::X4 => 16,
            leaf_code::X2 => 8,
            leaf_code::SECTOR => 4,
            leaf_code::HALF => 2,
            _ => 1,
        };
        // Wholly outside the room: nothing. Partly inside a big block:
        // must split.
        if x >= wx * C || z >= wz * C || x + n <= 0 || z + n <= 0 {
            continue;
        }
        let inside = x >= 0 && z >= 0 && x + n <= wx * C && z + n <= wz * C;
        let can_split = code != leaf_code::QUARTER;
        let split = !inside
            || (can_split && rng.range(0, 99) < if code == leaf_code::X4 { 55 } else { 45 });
        if split && can_split {
            let next = match code {
                leaf_code::X4 => leaf_code::X2,
                leaf_code::X2 => leaf_code::SECTOR,
                leaf_code::SECTOR => leaf_code::HALF,
                _ => leaf_code::QUARTER,
            };
            let h = n / 2;
            for (dx, dz) in [(0, 0), (h, 0), (0, h), (h, h)] {
                stack.push((x + dx, z + dz, next));
            }
            continue;
        }
        // Occasionally leave a hole (no leaf) in small leaves.
        if code >= leaf_code::HALF && rng.range(0, 99) < 12 {
            continue;
        }
        let floor = if rng.range(0, 2) == 0 {
            [rng.range(-20, 60) as i16; 4]
        } else {
            [0; 4].map(|_| rng.range(-20, 60) as i16)
        };
        let mut leaf = TLeaf::new(x, z, code, floor);
        if rng.range(0, 3) == 0 {
            leaf.sector.flags |= SECTOR_FLOOR_NESW;
        }
        room.leaves.push(leaf);
    }
    room
}

#[test]
fn lookup_matches_the_leaf_list_at_every_point() {
    for (seed, om) in [(1u32, (0, 0)), (2, (1, 3)), (3, (2, 2)), (4, (3, 1))] {
        let room = random_room(seed, 9, 7, om);
        assert!(room
            .leaves
            .iter()
            .any(|l| l.code == leaf_code::X2 || l.code == leaf_code::X4));
        assert!(room.leaves.iter().any(|l| l.code >= leaf_code::HALF));
        let built = tree_world(core::slice::from_ref(&room));
        built.run(|w| {
            let mut checked = 0;
            for z in 0..i32::from(room.d) * S {
                for x in 0..i32::from(room.w) * S {
                    let (cx, cz) = ((x / S) as usize, (z / S) as usize);
                    let (lx, lz) = ((x % S) as u32, (z % S) as u32);
                    let got = w.floor_at(0, cx, cz, lx, lz);
                    let leaf = room.leaf_at(x / Q, z / Q);
                    let want = leaf.and_then(|l| reference_floor(l, x, z));
                    assert_eq!(got, want, "seed {seed} om {om:?} at ({x}, {z})");
                    checked += 1;
                }
            }
            assert_eq!(checked, 9 * 7 * S * S);
        });
    }
}

/// Two dense rooms with slopes, pits, ceilings, split level and walls of
/// every flavour.
fn mixed_rooms() -> (Room, Room) {
    let mut a = Room::flat([0, 0, 0], 5, 5, 0);
    a.cell(1, 1).floor = [0, 20, 40, 20];
    a.cell(2, 1).floor = [30, 30, 80, 80];
    a.cell(3, 3).flags |= SECTOR_FLOOR_NESW;
    a.cell(3, 3).floor = [0, 10, 0, 10];
    a.set_floor(0, 4, 40);
    a.pit(2, 2);
    a.pit(4, 0);
    a.ceiling(1, 3, 90, true);
    a.ceiling(2, 3, 60, true);
    a.wall(1, 2, edge::NORTH, 0, 120);
    a.wall(3, 1, edge::NESW, 0, 120);
    a.wall(3, 4, edge::WEST, 0, 120);
    a.wall_flagged(0, 1, edge::EAST, 0, 200, WALL_FRONT_ONLY);
    a.wall_flagged(2, 0, edge::NWSE, 0, 200, WALL_BACK_ONLY);
    a.cell(4, 4).flags |= SECTOR_SPLIT_LEVEL;
    a.splits.push((
        24,
        SplitB {
            floor_b: [30; 4],
            ceil_b: [0; 4],
        },
    ));
    let mut b = Room::flat([0, 150, 0], 3, 3, 0);
    b.pit(1, 1);
    b.ceiling(0, 0, 80, true);
    (a, b)
}

#[test]
fn a_tree_encoded_dense_room_traces_like_the_dense_room() {
    let (a, b) = mixed_rooms();
    let dense_world = build_chunks(&[dense(&a), dense(&b)]);
    let tree_world = build_chunks(&[forced_tree(&a), forced_tree(&b)]);
    dense_world.run(|d| {
        tree_world.run(|t| {
            let mut rng = Lcg(777);
            for i in 0..6000 {
                let p = [
                    rng.range(-40, 440),
                    rng.range(-60, 300),
                    rng.range(-40, 440),
                ];
                let e = match i % 4 {
                    0 => [
                        rng.range(-40, 440),
                        rng.range(-60, 300),
                        rng.range(-40, 440),
                    ],
                    1 => [p[0], rng.range(-3000, 3000), p[2]],
                    2 => [rng.range(-40, 440), p[1], rng.range(-40, 440)],
                    _ => [
                        p[0] + rng.range(-30, 30),
                        p[1] + rng.range(-30, 30),
                        p[2] + rng.range(-30, 30),
                    ],
                };
                let query = match (i / 4) % 3 {
                    0 => point(p, e),
                    1 => body(p, e),
                    _ => CollisionTraceQuery::body(
                        RoomPoint::from_array(p),
                        RoomPoint::from_array(e),
                        6,
                        30,
                    ),
                };
                assert_eq!(
                    trace(d, query),
                    trace(t, query),
                    "query {i}: {p:?} -> {e:?}"
                );
                assert_eq!(
                    d.line_of_sight(p, e),
                    t.line_of_sight(p, e),
                    "sight {i}: {p:?} -> {e:?}"
                );
                assert_eq!(d.body_blocked(p, 12, 48), t.body_blocked(p, 12, 48));
                assert_eq!(d.locate(0, p), t.locate(0, p));
            }
        });
    });
}

/// A room of two sectors by one whose first sector holds `n` quarter wide
/// columns of rising floors (steps of `rise`).
fn stairs(rise: i16) -> TreeRoom {
    let mut room = TreeRoom::new([0, 0, 0], 2, 1, (0, 0));
    // First sector: four columns of quarter leaves, each four leaves deep.
    for qx in 0..4 {
        for qz in 0..4 {
            room.leaves.push(TLeaf::new(
                qx,
                qz,
                leaf_code::QUARTER,
                [rise * (qx as i16 + 1); 4],
            ));
        }
    }
    room.leaves
        .push(TLeaf::new(4, 0, leaf_code::SECTOR, [rise * 4; 4]));
    room
}

#[test]
fn quarter_leaf_treads_collide_one_step_at_a_time() {
    let room = stairs(8);
    let built = tree_world(core::slice::from_ref(&room));
    built.run(|w| {
        // The floor is a staircase, not a ramp.
        for tread in 0..4 {
            let x = tread * Q + Q / 2;
            assert_eq!(
                w.floor_at(0, 0, 0, x as u32, 30),
                Some(8 * (tread as i32 + 1))
            );
        }
        // A probe standing over each tread lands exactly on it.
        for tread in 0..4 {
            let x = tread * Q + 3;
            let t = trace(w, body([x, 100, 40], [x, -100, 40]));
            assert_eq!(t.end.y, 8 * (tread + 1), "tread {tread}: {t:?}");
            assert_eq!(t.normal_q12, [0, 4096, 0]);
        }
        // Standing on one tread, the next is blocked and the previous is not.
        assert!(!w.body_blocked([Q / 2, 8, 40], 3, 48));
        assert!(w.body_blocked([Q + Q / 2, 8, 40], 3, 48));
        assert!(!w.body_blocked([Q + Q / 2, 16, 40], 3, 48));
        assert!(!w.body_blocked([Q / 2, 16, 40], 3, 48));
        // A horizontal body trace into the riser stops short of it (the
        // footprint's far sample reaches the next tread, so the centre
        // stays within the radius of the edge). The contact normal follows
        // the dense rule: the centre did not leave its cell.
        let t = trace(
            w,
            CollisionTraceQuery::body(
                RoomPoint::new(Q - 10, 8, 40),
                RoomPoint::new(2 * Q, 8, 40),
                3,
                48,
            ),
        );
        assert!(t.fraction_q12 < 4096, "{t:?}");
        assert!(t.end.x <= Q - 3 && t.end.x >= Q - 5, "{t:?}");
        // A point ray along the treads at slab height enters the slab of the
        // second tread at its riser and reports a horizontal normal.
        let t = trace(w, point([2, 12, 30], [S - 2, 12, 30]));
        assert!(t.fraction_q12 < 4096, "{t:?}");
        assert!(t.end.x >= Q - 1 && t.end.x <= Q, "{t:?}");
        assert_eq!(t.normal_q12, [-4096, 0, 0], "{t:?}");
        // A ray from above lands on the right tread.
        let t = trace(w, point([2 * Q + 5, 90, 30], [2 * Q + 5, -90, 30]));
        assert_eq!(t.end.y, 24);
        assert_eq!(t.normal_q12, [0, 4096, 0]);
    });
}

#[test]
fn a_big_leaf_with_an_offset_origin_is_one_flat_record() {
    // World origin one sector into a four sector block: the room's sector 3
    // starts a block.
    let mut room = TreeRoom::new([0, 0, 0], 7, 1, (1, 0));
    // Room sectors 0..3 are world 1..4: world block 0 holds world 1,2,3 and
    // block 4 holds world 4..: leaves of two sectors are aligned to world 0,
    // 2, 4, so room sectors 1..2 (world 2,3) form one.
    room.leaves
        .push(TLeaf::new(0, 0, leaf_code::SECTOR, [10; 4]));
    room.leaves.push(TLeaf::new(4, 0, leaf_code::X2, [20; 4]));
    room.leaves.push(TLeaf::new(12, 0, leaf_code::X4, [30; 4]));
    // The 4-sector leaf above covers room sectors 3..6 only if its world
    // start (4) is a multiple of 4: room sector 3 is world 4. Yes.
    let built = tree_world(core::slice::from_ref(&room));
    built.run(|w| {
        assert_eq!(w.floor_at(0, 0, 0, 5, 5), Some(10));
        for cx in 1..3 {
            assert_eq!(w.floor_at(0, cx, 0, 3, 70), Some(20));
        }
        for cx in 3..7 {
            assert_eq!(w.floor_at(0, cx, 0, 79, 0), Some(30));
        }
        let t = trace(w, body([4 * S + 10, 100, 40], [4 * S + 10, -100, 40]));
        assert_eq!(t.end.y, 30);
        // The record count is the leaf count.
        assert_eq!(room.chunk().sectors.len(), 3);
    });
}

#[test]
fn walls_on_leaves_use_the_leaf_frame_and_half_spans() {
    let mut room = TreeRoom::new([0, 0, 0], 4, 4, (0, 0));
    let mut big = TLeaf::new(0, 0, leaf_code::X4, [0; 4]);
    big.walls.push(ColWall {
        edge: edge::SOUTH,
        flags: 0,
        y: [0, 0, 100, 100],
        reserved: 0,
    });
    big.walls.push(ColWall {
        edge: edge::NORTH,
        flags: WALL_HALF_SECOND,
        y: [0, 0, 100, 100],
        reserved: 0,
    });
    room.leaves.push(big);
    let built = tree_world(core::slice::from_ref(&room));
    built.run(|w| {
        let edge_z = 4 * S;
        // The south wall is at the far edge of the leaf, whichever sector.
        for x in [10, 150, 300] {
            assert!(w.body_blocked([x, 0, edge_z - 5], 12, 48), "x {x}");
            assert!(!w.body_blocked([x, 0, edge_z - 20], 12, 48), "x {x}");
            let t = trace(w, point([x, 10, edge_z - 40], [x, 10, edge_z + 40]));
            assert!(t.fraction_q12 < 4096 && t.end.z <= edge_z, "x {x}: {t:?}");
        }
        // The north wall covers the half walking east: x in 2S..4S (north
        // edges walk west to east, so the second half is the east half).
        assert!(!w.body_blocked([S, 0, 5], 12, 48));
        assert!(w.body_blocked([3 * S, 0, 5], 12, 48));
        let t = trace(w, point([3 * S, 10, 40], [3 * S, 10, -40]));
        assert!(t.fraction_q12 < 4096, "{t:?}");
        let t = trace(w, point([S, 10, 40], [S, 10, -40]));
        assert_eq!(t.fraction_q12, 4096, "{t:?}");
    });
}

#[test]
fn walls_on_a_quarter_leaf_and_on_a_half_leaf() {
    let mut room = TreeRoom::new([0, 0, 0], 2, 1, (0, 0));
    // Sector 0: a half leaf (2 by 2 clicks) at the NW with a wall on its
    // east edge, the rest quarter leaves.
    let mut half = TLeaf::new(0, 0, leaf_code::HALF, [0; 4]);
    half.walls.push(ColWall {
        edge: edge::EAST,
        flags: 0,
        y: [0, 0, 100, 100],
        reserved: 0,
    });
    room.leaves.push(half);
    for (x, z) in [(2, 0), (3, 0), (2, 1), (3, 1)] {
        room.leaves
            .push(TLeaf::new(x, z, leaf_code::QUARTER, [0; 4]));
    }
    room.leaves.push(TLeaf::new(0, 2, leaf_code::HALF, [0; 4]));
    for (x, z) in [(2, 2), (3, 2), (2, 3), (3, 3)] {
        let mut leaf = TLeaf::new(x, z, leaf_code::QUARTER, [0; 4]);
        if (x, z) == (3, 3) {
            leaf.walls.push(ColWall {
                edge: edge::SOUTH,
                flags: 0,
                y: [0, 0, 100, 100],
                reserved: 0,
            });
        }
        room.leaves.push(leaf);
    }
    room.leaves
        .push(TLeaf::new(4, 0, leaf_code::SECTOR, [0; 4]));
    let built = tree_world(core::slice::from_ref(&room));
    built.run(|w| {
        // The half leaf spans x 0..S/2, z 0..S/2; its east wall is at S/2,
        // from z 0 to S/2 only.
        assert!(w.body_blocked([S / 2 - 5, 0, 10], 12, 48));
        assert!(!w.body_blocked([S / 2 - 25, 0, 10], 12, 48));
        assert!(!w.body_blocked([S / 2 - 5, 0, S / 2 + 30], 12, 48));
        let t = trace(w, point([10, 10, 20], [S - 10, 10, 20]));
        assert!(t.fraction_q12 < 4096 && t.end.x <= S / 2, "{t:?}");
        let t = trace(w, point([10, 10, S - 5], [S - 10, 10, S - 5]));
        assert_eq!(t.fraction_q12, 4096, "{t:?}");
        // The quarter leaf's south wall is at z = S, x from 3Q to S.
        assert!(w.body_blocked([3 * Q + 10, 0, S - 5], 12, 48));
        assert!(!w.body_blocked([Q + 10, 0, S - 5], 12, 48));
    });
}

#[test]
fn random_trees_never_end_inside_solid() {
    let room = random_room(9, 8, 8, (2, 1));
    let built = tree_world(core::slice::from_ref(&room));
    built.run(|w| {
        let mut rng = Lcg(31337);
        for i in 0..4000 {
            let a = [
                rng.range(-40, 8 * S + 40),
                rng.range(-60, 120),
                rng.range(-40, 8 * S + 40),
            ];
            let b = match i % 3 {
                0 => [a[0], rng.range(-3000, 3000), a[2]],
                1 => [
                    a[0] + rng.range(-60, 60),
                    a[1] + rng.range(-30, 30),
                    a[2] + rng.range(-60, 60),
                ],
                _ => [
                    rng.range(-40, 8 * S + 40),
                    rng.range(-60, 120),
                    rng.range(-40, 8 * S + 40),
                ],
            };
            let (r, h) = if (i / 3) % 2 == 0 { (0, 0) } else { (6, 30) };
            let query = if r == 0 {
                point(a, b)
            } else {
                CollisionTraceQuery::body(RoomPoint::from_array(a), RoomPoint::from_array(b), r, h)
            };
            let t = trace(w, query);
            if t.start_solid || t.fraction_q12 == 0 {
                continue;
            }
            if r == 0 {
                assert!(!w.point_in_solid(t.end.to_array()), "{a:?} -> {b:?}: {t:?}");
            } else {
                assert!(
                    !w.body_blocked(t.end.to_array(), r, h),
                    "{a:?} -> {b:?}: {t:?}"
                );
            }
        }
    });
}
