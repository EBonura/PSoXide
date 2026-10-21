use super::*;
use crate::format::*;
use crate::resident::ResidentRooms;
use crate::view::{CollisionView, LevelView};
use crate::write::{aligned_copy, words_as_bytes, LevelCore, RoomChunk};
use alloc::vec;
use alloc::vec::Vec;
use psx_engine::{
    Angle, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, CollisionTrace,
    CollisionTraceProvider, CollisionTraceQuery, RoomPoint, Q12,
};

mod lattice;
mod leaf;
mod spans;

const S: i32 = 80;
const FLAT: u16 = SECTOR_HAS_FLOOR | SECTOR_FLOOR_WALK_A | SECTOR_FLOOR_WALK_B;

#[derive(Clone)]
pub(super) struct Room {
    origin: [i32; 3],
    w: u8,
    d: u8,
    sectors: Vec<Sector>,
    walls: Vec<(usize, ColWall)>,
    splits: Vec<(usize, SplitB)>,
    liquids: Vec<Liquid>,
    /// Free planes; each is copied into every cell its footprint box touches.
    pub(super) planes: Vec<ColPlane>,
}

impl Room {
    pub(super) fn flat(origin: [i32; 3], w: u8, d: u8, floor: i16) -> Self {
        let cell = Sector {
            floor: [floor; 4],
            ceil: [0; 4],
            flags: FLAT,
            ext: NO_EXT,
        };
        Self {
            origin,
            w,
            d,
            sectors: vec![cell; usize::from(w) * usize::from(d)],
            walls: Vec::new(),
            splits: Vec::new(),
            liquids: Vec::new(),
            planes: Vec::new(),
        }
    }

    pub(super) fn cell(&mut self, cx: usize, cz: usize) -> &mut Sector {
        let w = usize::from(self.w);
        &mut self.sectors[cz * w + cx]
    }

    pub(super) fn set_floor(&mut self, cx: usize, cz: usize, h: i16) {
        self.cell(cx, cz).floor = [h; 4];
    }

    pub(super) fn pit(&mut self, cx: usize, cz: usize) {
        self.cell(cx, cz).flags &= !SECTOR_HAS_FLOOR;
    }

    fn ceiling(&mut self, cx: usize, cz: usize, h: i16, solid: bool) {
        let c = self.cell(cx, cz);
        c.ceil = [h; 4];
        c.flags |= SECTOR_HAS_CEIL;
        if solid {
            c.flags |= SECTOR_CEIL_SOLID;
        }
    }

    fn wall(&mut self, cx: usize, cz: usize, edge: u8, bottom: i16, top: i16) {
        self.wall_flagged(cx, cz, edge, bottom, top, 0);
    }

    fn wall_flagged(&mut self, cx: usize, cz: usize, edge: u8, bottom: i16, top: i16, flags: u8) {
        let index = cz * usize::from(self.w) + cx;
        self.walls.push((
            index,
            ColWall {
                edge,
                flags,
                y: [bottom, bottom, top, top],
                reserved: 0,
            },
        ));
    }

    /// Plane indices whose bounding box touches cell `index`.
    fn planes_in(&self, index: usize) -> Vec<usize> {
        let w = usize::from(self.w);
        let (cx, cz) = ((index % w) as i32 * S, (index / w) as i32 * S);
        (0..self.planes.len())
            .filter(|&i| {
                let p = &self.planes[i];
                let xs = p.xz.map(|c| i32::from(c[0]));
                let zs = p.xz.map(|c| i32::from(c[1]));
                let (x0, x1) = (xs.iter().min().unwrap(), xs.iter().max().unwrap());
                let (z0, z1) = (zs.iter().min().unwrap(), zs.iter().max().unwrap());
                *x1 >= cx && *x0 <= cx + S && *z1 >= cz && *z0 <= cz + S
            })
            .collect()
    }

    fn chunk(&self, summary: bool) -> RoomChunk {
        let mut sectors = self.sectors.clone();
        let mut exts: Vec<SectorExt> = Vec::new();
        let mut walls = Vec::new();
        let mut splitbs = Vec::new();
        let mut col_planes: Vec<ColPlane> = Vec::new();
        for (index, sector) in sectors.iter_mut().enumerate() {
            let mine: Vec<ColWall> = self
                .walls
                .iter()
                .filter(|(i, _)| *i == index)
                .map(|(_, w)| *w)
                .collect();
            let split = self.splits.iter().find(|(i, _)| *i == index);
            let planes = self.planes_in(index);
            if mine.is_empty() && split.is_none() && planes.is_empty() {
                continue;
            }
            let mut ext = SectorExt {
                wall_first: walls.len() as u16,
                wall_count: mine.len() as u8,
                plane_count: planes.len() as u8,
                plane_first: col_planes.len() as u16,
                splitb: NO_EXT,
            };
            for i in planes {
                let mut copy = self.planes[i];
                copy.id = i as u8;
                let first = !col_planes.iter().any(|p| p.id == copy.id);
                copy.flags =
                    (copy.flags & !COLPLANE_PRIMARY) | if first { COLPLANE_PRIMARY } else { 0 };
                col_planes.push(copy);
            }
            walls.extend(mine);
            if let Some((_, b)) = split {
                ext.splitb = splitbs.len() as u16;
                splitbs.push(*b);
            }
            sector.ext = exts.len() as u16;
            exts.push(ext);
        }
        if summary {
            summarize_sectors(&mut sectors, usize::from(self.w), usize::from(self.d));
        }
        RoomChunk {
            width: self.w,
            depth: self.d,
            sectors,
            exts,
            walls,
            splitbs,
            col_planes,
            liquids: self.liquids.clone(),
            ..RoomChunk::default()
        }
    }
}

pub(super) struct Built {
    level: Vec<u32>,
    level_len: usize,
    rooms: Vec<(Vec<u32>, usize)>,
}

pub(super) fn build(rooms: &[Room]) -> Built {
    build_with(rooms, true)
}

/// Build with or without the derived sector summary bits: the two must trace
/// identically.
fn build_with(rooms: &[Room], summary: bool) -> Built {
    let core = LevelCore {
        occupancy: Vec::new(),
        sector_units: S as u16,
        rooms: rooms
            .iter()
            .map(|r| RoomRecord {
                origin: r.origin,
                width: r.w,
                depth: r.d,
                min_y: -200,
                max_y: 400,
                ..RoomRecord::default()
            })
            .collect(),
        ..LevelCore::default()
    };
    let bytes = core.to_bytes();
    let chunks = rooms
        .iter()
        .map(|r| {
            let b = r.chunk(summary).to_bytes();
            (aligned_copy(&b), b.len())
        })
        .collect();
    Built {
        level: aligned_copy(&bytes),
        level_len: bytes.len(),
        rooms: chunks,
    }
}

impl Built {
    fn run_with<R>(&self, resident: &[bool], f: impl FnOnce(&GridCollision<'_>) -> R) -> R {
        let level = LevelView::parse(words_as_bytes(&self.level, self.level_len)).unwrap();
        let views: Vec<Option<CollisionView<'_>>> = self
            .rooms
            .iter()
            .enumerate()
            .map(|(i, (w, l))| {
                resident
                    .get(i)
                    .copied()
                    .unwrap_or(true)
                    .then(|| CollisionView::parse(words_as_bytes(w, *l)).unwrap())
            })
            .collect();
        let set = ResidentRooms::from_collisions(level, &views).unwrap();
        let world = set.collision();
        f(&world)
    }

    pub(super) fn run<R>(&self, f: impl FnOnce(&GridCollision<'_>) -> R) -> R {
        self.run_with(&[], f)
    }
}

pub(super) fn trace(w: &GridCollision<'_>, q: CollisionTraceQuery) -> CollisionTrace {
    let mut out = CollisionTrace::default();
    assert!(GridTraceProvider::new(w).trace_into(q, &mut out));
    out
}

pub(super) fn body(a: [i32; 3], b: [i32; 3]) -> CollisionTraceQuery {
    CollisionTraceQuery::body(RoomPoint::from_array(a), RoomPoint::from_array(b), 12, 48)
}

pub(super) fn point(a: [i32; 3], b: [i32; 3]) -> CollisionTraceQuery {
    CollisionTraceQuery::point(RoomPoint::from_array(a), RoomPoint::from_array(b))
}

#[test]
fn flat_floor_stand_probe_and_fall() {
    let mut room = Room::flat([0, 0, 0], 4, 2, 0);
    room.pit(3, 1);
    let built = build(&[room]);
    built.run(|w| {
        // Motor-style near probe from 1 above the feet.
        let t = trace(w, body([100, 1, 60], [100, -39, 60]));
        assert!(!t.start_solid && !t.all_solid);
        assert_eq!(t.end.y, 0);
        assert_eq!(t.fraction_q12, 4096 / 40);
        assert_eq!(t.normal_q12, [0, 4096, 0]);
        // Far probe.
        let t = trace(w, body([100, 1, 60], [100, 1 - 32767, 60]));
        assert_eq!(t.end.y, 0);
        assert!(t.normal_q12[1] >= 2867);
        // Standing exactly on the surface is not solid.
        assert!(!w.body_blocked([100, 0, 60], 12, 48));
        // The pit has no floor: bodies fall through.
        let t = trace(w, body([300, 50, 120], [300, -5000, 120]));
        assert_eq!(
            t,
            CollisionTrace::unobstructed(RoomPoint::new(300, -5000, 120))
        );
        // Inside the floor slab: start solid, free end.
        let t = trace(w, body([100, -5, 60], [100, 60, 60]));
        assert!(t.start_solid && !t.all_solid);
        assert_eq!(t.fraction_q12, 4096);
        let t = trace(w, body([100, -5, 60], [100, -8, 60]));
        assert!(t.all_solid && t.start_solid);
        assert_eq!(t.fraction_q12, 0);
        assert_eq!(t.end, RoomPoint::new(100, -5, 60));
        // A point trace down.
        let t = trace(w, point([20, 90, 20], [20, -9000, 20]));
        assert_eq!(t.end.y, 0);
    });
}

#[test]
fn slope_normal_and_walkable_flag() {
    let mut room = Room::flat([0, 0, 0], 2, 2, 0);
    // NW 0, NE 0, SE 20, SW 20: rises toward +Z.
    room.cell(0, 0).floor = [0, 0, 20, 20];
    room.cell(1, 0).floor = [0, 0, 100, 100];
    let built = build(&[room]);
    built.run(|w| {
        let t = trace(w, point([40, 60, 40], [40, -60, 40]));
        assert!((t.end.y - 10).abs() <= 1, "{:?}", t.end);
        assert!(t.normal_q12[1] >= 2867, "{:?}", t.normal_q12);
        assert!(t.normal_q12[2] < 0);
        let t = trace(w, point([120, 160, 40], [120, -60, 40]));
        assert!(
            t.normal_q12[1] < 2867 && t.normal_q12[1] > 0,
            "{:?}",
            t.normal_q12
        );
        // The body probe rests on the highest footprint sample.
        let t = trace(w, body([40, 100, 40], [40, -100, 40]));
        let up = w.floor_at(0, 0, 0, 40, 52).unwrap();
        assert_eq!(t.end.y, up);
    });
}

#[test]
fn split_level_uses_second_triangle_heights() {
    let mut room = Room::flat([0, 0, 0], 1, 1, 0);
    room.cell(0, 0).flags |= SECTOR_SPLIT_LEVEL;
    room.splits.push((
        0,
        SplitB {
            floor_b: [30; 4],
            ceil_b: [0; 4],
        },
    ));
    let built = build(&[room]);
    built.run(|w| {
        assert_eq!(w.floor_at(0, 0, 0, 60, 20), Some(0));
        assert_eq!(w.floor_at(0, 0, 0, 20, 60), Some(30));
        let t = trace(w, point([20, 100, 60], [20, -100, 60]));
        assert_eq!(t.end.y, 30);
    });
}

#[test]
fn ceiling_blocks_the_head() {
    let mut room = Room::flat([0, 0, 0], 2, 1, 0);
    room.ceiling(0, 0, 100, true);
    room.ceiling(1, 0, 100, false);
    let built = build(&[room]);
    built.run(|w| {
        let t = trace(w, body([40, 0, 40], [40, 300, 40]));
        assert_eq!(t.end.y, 100 - 48);
        assert_eq!(t.normal_q12, [0, -4096, 0]);
        // A non-solid ceiling does not block.
        let t = trace(w, body([120, 0, 40], [120, 300, 40]));
        assert_eq!(t.fraction_q12, 4096);
        // Horizontal body sweep into a low ceiling is blocked.
        let t = trace(w, body([120, 60, 40], [40, 60, 40]));
        assert!(t.fraction_q12 < 4096);
    });
}

#[test]
fn walls_block_bodies_and_points() {
    let mut room = Room::flat([0, 0, 0], 4, 2, 0);
    room.wall(1, 0, edge::EAST, 0, 200);
    room.wall(2, 1, edge::NWSE, 0, 200);
    let built = build(&[room]);
    built.run(|w| {
        // Body: wall at x = 160, radius 12.
        let t = trace(w, body([100, 0, 40], [200, 0, 40]));
        assert!(t.fraction_q12 < 4096);
        assert!(t.end.x <= 148 && t.end.x >= 146, "{:?}", t.end);
        assert_eq!(t.normal_q12, [-4096, 0, 0]);
        // Touching is clear.
        assert!(!w.body_blocked([148, 0, 40], 12, 48));
        assert!(w.body_blocked([149, 0, 40], 12, 48));
        // Above the wall: clear.
        assert!(!w.body_blocked([160, 200, 40], 12, 48));
        // Point: thin wall is not tunnelled, even in one huge step.
        let t = trace(w, point([100, 10, 40], [3000, 10, 40]));
        assert!(t.end.x <= 160 && t.end.x > 150, "{:?}", t.end);
        assert_eq!(t.normal_q12, [-4096, 0, 0]);
        let t = trace(w, point([100, 10, 40], [220, 10, 41]));
        assert!(t.fraction_q12 < 4096);
        // Over the top of the wall the point passes.
        let t = trace(w, point([100, 250, 40], [220, 250, 40]));
        assert_eq!(t.fraction_q12, 4096);
        // Diagonal wall in cell (2,1): x = z locally.
        let (ox, oz) = (2 * S, S);
        let t = trace(w, point([ox + 60, 10, oz + 20], [ox + 20, 10, oz + 60]));
        assert!(t.fraction_q12 < 4096 && t.fraction_q12 > 1500, "{:?}", t);
        assert_eq!(t.normal_q12, [2896, 0, -2896]);
        let t = trace(w, body([ox + 60, 0, oz + 20], [ox + 20, 0, oz + 60]));
        assert!(t.fraction_q12 < 4096, "{:?}", t);
        assert_eq!(t.normal_q12, [2896, 0, -2896]);
        // Body clear of the diagonal by more than its radius.
        assert!(!w.body_blocked([ox + 60, 0, oz + 20], 12, 48));
        assert!(w.body_blocked([ox + 45, 0, oz + 35], 12, 48));
        // Vertical movement next to a wall is unaffected unless it overlaps.
        let t = trace(w, body([140, 1, 40], [140, -40, 40]));
        assert_eq!(t.end.y, 0);
    });
}

#[test]
fn stacked_rooms_pick_the_right_floor() {
    let mut a = Room::flat([0, 0, 0], 2, 2, 0);
    for cz in 0..2 {
        for cx in 0..2 {
            a.ceiling(cx, cz, 100, false);
        }
    }
    let b = Room::flat([0, 200, 0], 2, 2, 0);
    let built = build(&[a, b]);
    built.run(|w| {
        let low = w.locate(1, [40, 50, 40]).unwrap();
        assert_eq!(
            (low.room, low.cx, low.cz, low.lx, low.lz),
            (0, 0, 0, 40, 40)
        );
        let high = w.locate(0, [100, 250, 100]).unwrap();
        assert_eq!((high.room, high.cx, high.cz), (1, 1, 1));
        // Just below room B's floor slab and above A's ceiling: nearest wins,
        // hint breaks the tie.
        assert!(w.locate(0, [40, 150, 40]).is_some());
        assert!(w.locate(0, [-5, 50, 40]).is_none());
        // Probes pick the right floor by y.
        let t = trace(w, body([40, 301, 40], [40, -5000, 40]));
        assert_eq!(t.end.y, 200);
        let t = trace(w, body([40, 130, 40], [40, -5000, 40]));
        assert_eq!(t.end.y, 0, "{:?}", t);
    });
}

#[test]
fn line_of_sight_through_cells_walls_floors_and_ceilings() {
    let mut room = Room::flat([0, 0, 0], 4, 2, 0);
    room.wall(1, 0, edge::EAST, 0, 100);
    room.ceiling(3, 0, 120, true);
    let built = build(&[room]);
    built.run(|w| {
        assert!(!w.line_of_sight([10, 30, 10], [300, 30, 30]));
        assert!(w.line_of_sight([10, 30, 70], [300, 30, 150]));
        // Behind the wall at x = 160 (z within the cell row 0).
        assert!(!w.line_of_sight([10, 30, 40], [300, 30, 40]));
        // Over the wall.
        assert!(w.line_of_sight([10, 110, 40], [300, 110, 40]));
        // Through the floor.
        assert!(!w.line_of_sight([10, 30, 100], [10, -30, 100]));
        assert!(!w.line_of_sight([10, 30, 100], [200, -40, 100]));
        // Endpoint exactly on the surface does not block.
        assert!(w.line_of_sight([10, 30, 100], [10, 0, 100]));
        assert!(w.line_of_sight([10, 0, 100], [200, 0, 100]));
        // Starting inside the slab blocks.
        assert!(!w.line_of_sight([10, -5, 100], [200, 30, 100]));
        // Solid ceiling.
        assert!(!w.line_of_sight([260, 60, 40], [260, 200, 40]));
        assert!(w.line_of_sight([200, 60, 100], [200, 200, 100]));
        // Out-of-range input is clamped, never panics.
        let _ = w.line_of_sight([i32::MIN, i32::MAX, 0], [i32::MAX, i32::MIN, 5]);
    });
}

#[test]
fn contents_reports_liquid_volumes() {
    let mut room = Room::flat([0, 0, 0], 3, 1, 0);
    room.liquids.push(Liquid {
        x0: 1,
        z0: 0,
        x1: 1,
        z1: 0,
        kind: liquid_kind::LAVA,
        flags: 0,
        surface_y: 30,
        bottom_y: -20,
        reserved: 0,
    });
    let built = build(&[room]);
    built.run(|w| {
        assert_eq!(w.contents_at([100, 10, 40]), liquid_kind::LAVA);
        assert_eq!(w.contents_at([100, 30, 40]), liquid_kind::LAVA);
        assert_eq!(w.contents_at([100, 31, 40]), 0);
        assert_eq!(w.contents_at([100, -21, 40]), 0);
        assert_eq!(w.contents_at([20, 10, 40]), 0);
        assert_eq!(w.contents_at([500, 10, 40]), 0);
    });
}

#[test]
fn non_resident_rooms_are_solid_for_bodies_and_invisible_to_sight() {
    let a = Room::flat([0, 0, 0], 2, 1, 0);
    let b = Room::flat([160, 0, 0], 2, 1, 0);
    let built = build(&[a, b]);
    built.run_with(&[true, false], |w| {
        assert!(w.body_blocked([200, 20, 40], 12, 48));
        assert!(!w.body_blocked([100, 20, 40], 12, 48));
        assert!(w.line_of_sight([10, 20, 40], [300, 20, 40]));
        let t = trace(w, body([100, 0, 40], [300, 0, 40]));
        assert!(t.fraction_q12 < 4096);
    });
}

pub(super) struct Lcg(pub(super) u32);

impl Lcg {
    pub(super) fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 8
    }

    pub(super) fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u32) as i32
    }
}

fn property_world() -> Built {
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
    let mut b = Room::flat([0, 150, 0], 3, 3, 0);
    b.pit(1, 1);
    b.ceiling(0, 0, 80, true);
    build(&[a, b])
}

#[test]
fn traces_never_end_inside_solid() {
    let built = property_world();
    built.run(|w| {
        let mut rng = Lcg(12345);
        let mut hits = 0;
        for i in 0..4000 {
            let a = [
                rng.range(-40, 440),
                rng.range(-60, 300),
                rng.range(-40, 440),
            ];
            let b = match i % 4 {
                0 => [
                    rng.range(-40, 440),
                    rng.range(-60, 300),
                    rng.range(-40, 440),
                ],
                1 => [a[0], rng.range(-3000, 3000), a[2]],
                2 => [rng.range(-40, 440), a[1], rng.range(-40, 440)],
                _ => [
                    a[0] + rng.range(-30, 30),
                    a[1] + rng.range(-30, 30),
                    a[2] + rng.range(-30, 30),
                ],
            };
            let (shape_r, shape_h) = match (i / 4) % 3 {
                0 => (0, 0),
                1 => (12, 48),
                _ => (6, 30),
            };
            let query = if shape_r == 0 {
                point(a, b)
            } else {
                CollisionTraceQuery::body(
                    RoomPoint::from_array(a),
                    RoomPoint::from_array(b),
                    shape_r,
                    shape_h,
                )
            };
            let t = trace(w, query);
            let blocked = |p: [i32; 3]| {
                if shape_r == 0 {
                    w.point_in_solid(p)
                } else {
                    w.body_blocked(p, shape_r, shape_h)
                }
            };
            assert!((0..=4096).contains(&t.fraction_q12), "{query:?} {t:?}");
            if t.all_solid {
                assert!(t.start_solid && t.fraction_q12 == 0 && t.end == query.start);
                assert!(blocked(a) && blocked(b));
            } else if t.start_solid {
                assert!(blocked(a) && !blocked(b));
                assert_eq!(t.end, query.end);
                assert_eq!(t.fraction_q12, 4096);
            } else {
                assert!(!blocked(a), "{query:?}");
                assert!(!blocked(t.end.to_array()), "{query:?} -> {t:?}");
                if t.fraction_q12 == 4096 {
                    assert_eq!(t.end, query.end);
                } else {
                    hits += 1;
                }
            }
        }
        assert!(
            hits > 500,
            "the property world must produce many hits: {hits}"
        );
    });
}

pub(super) fn motor_config() -> CharacterMotorConfig {
    CharacterMotorConfig::character_with_body(12, 48, 8 << 8, 8 << 8, Angle::from_q12(1024))
}

pub(super) fn walk(
    world: &GridCollision<'_>,
    start: RoomPoint,
    move_x: i32,
    move_z: i32,
    ticks: u32,
) -> RoomPoint {
    let mut motor = CharacterMotorState::new(start, Angle::ZERO);
    let mut provider = GridTraceProvider::new(world);
    let input = CharacterMotorInput {
        move_x: Q12::from_raw(move_x),
        move_z: Q12::from_raw(move_z),
        ..CharacterMotorInput::default()
    };
    for _ in 0..ticks {
        motor
            .update_vblanks_with_trace_provider(&mut provider, input, motor_config(), 1)
            .expect("provider never fails in range");
    }
    motor.position()
}

#[test]
fn motor_walks_flat_climbs_stairs_and_stops_at_risers() {
    // Cells along X: 0..3 flat, then steps of 20, then a long run.
    let mut stairs = Room::flat([0, 0, 0], 10, 1, 0);
    stairs.set_floor(3, 0, 20);
    stairs.set_floor(4, 0, 40);
    for cx in 5..10 {
        stairs.set_floor(cx, 0, 40);
    }
    let mut tall = Room::flat([0, 0, 0], 10, 1, 0);
    for cx in 4..10 {
        tall.set_floor(cx, 0, 60);
    }
    let built = build(&[stairs]);
    built.run(|w| {
        let end = walk(w, RoomPoint::new(20, 0, 40), 4096, 0, 20);
        assert_eq!(end.y, 0);
        assert!(end.x > 100, "{end:?}");
        let end = walk(w, RoomPoint::new(20, 0, 40), 4096, 0, 100);
        assert_eq!(end.y, 40, "{end:?}");
        assert!(end.x > 5 * S, "{end:?}");
    });
    let built = build(&[tall]);
    built.run(|w| {
        let end = walk(w, RoomPoint::new(20, 0, 40), 4096, 0, 100);
        assert_eq!(end.y, 0, "a 60 unit riser must block: {end:?}");
        assert!(end.x <= 4 * S - 12, "{end:?}");
        assert!(end.x > 4 * S - 12 - 10, "{end:?}");
    });
    let mut low = Room::flat([0, 0, 0], 10, 1, 0);
    for cx in 4..10 {
        low.set_floor(cx, 0, 20);
    }
    let built = build(&[low]);
    built.run(|w| {
        let end = walk(w, RoomPoint::new(20, 0, 40), 4096, 0, 100);
        assert_eq!(end.y, 20, "a 20 unit riser is stepped: {end:?}");
        assert!(end.x > 6 * S, "{end:?}");
    });
}

#[test]
fn motor_slides_along_walls_and_falls_off_ledges() {
    let mut room = Room::flat([0, 0, 0], 8, 3, 0);
    for cz in 0..3 {
        room.wall(4, cz, edge::WEST, 0, 200);
    }
    let built = build(&[room]);
    built.run(|w| {
        // Walk into the wall at x = 320 while drifting along +Z.
        let end = walk(w, RoomPoint::new(100, 0, 20), 4096, 1500, 120);
        assert!(end.x <= 4 * S - 12, "must not pass the wall: {end:?}");
        assert!(end.x > 4 * S - 12 - 10, "{end:?}");
        assert!(end.z > 60, "must slide along it: {end:?}");
        assert_eq!(end.y, 0);
    });
    let mut ledge = Room::flat([0, 0, 0], 8, 3, 0);
    for cx in 5..8 {
        for cz in 0..3 {
            ledge.set_floor(cx, cz, -200);
        }
    }
    let built = build(&[ledge]);
    built.run(|w| {
        let end = walk(w, RoomPoint::new(100, 0, 100), 4096, 0, 150);
        assert!(end.x > 5 * S, "{end:?}");
        assert_eq!(
            end.y, -200,
            "must fall off the ledge onto the lower floor: {end:?}"
        );
    });
}

fn summary_world(summary: bool) -> Built {
    let mut a = Room::flat([0, 0, 0], 8, 8, 0);
    a.set_floor(1, 6, 25);
    a.pit(6, 1);
    a.wall(5, 5, edge::EAST, 0, 120);
    a.wall(2, 2, edge::NWSE, 0, 120);
    a.cell(3, 6).floor = [0, 10, 0, 10];
    // A solid ceiling over a block of cells, high enough to keep sectors
    // clear, plus a low one that spoils its neighbourhood.
    for cz in 4..8 {
        for cx in 0..3 {
            a.ceiling(cx, cz, 220, true);
        }
    }
    a.ceiling(6, 6, 90, true);
    a.ceiling(7, 3, 300, false);
    let mut b = Room::flat([0, 260, 0], 4, 4, 0);
    b.pit(2, 2);
    build_with(&[a, b], summary)
}

#[test]
fn sector_summary_bits_do_not_change_any_trace() {
    let with = summary_world(true);
    let without = summary_world(false);
    // The summary must actually mark clear sectors, or this proves nothing.
    with.run(|w| {
        let clear = w
            .view(0)
            .unwrap()
            .sectors
            .iter()
            .filter(|s| s.flags & SECTOR_CLEAR != 0)
            .count();
        assert!(clear >= 3, "clear sectors: {clear}");
        let flat = w
            .view(0)
            .unwrap()
            .sectors
            .iter()
            .filter(|s| s.flags & SECTOR_FLAT_FLOOR != 0)
            .count();
        assert!(flat > 50);
    });
    without.run(|w| {
        assert!(w
            .view(0)
            .unwrap()
            .sectors
            .iter()
            .all(|s| s.flags & (SECTOR_CLEAR | SECTOR_FLAT_FLOOR | SECTOR_FLAT_CEIL) == 0));
    });
    let mut rng = Lcg(777);
    let mut queries = Vec::new();
    for i in 0..3000 {
        let a = [
            rng.range(-40, 680),
            rng.range(-60, 360),
            rng.range(-40, 680),
        ];
        let b = match i % 4 {
            0 => [
                rng.range(-40, 680),
                rng.range(-60, 360),
                rng.range(-40, 680),
            ],
            1 => [a[0], rng.range(-3000, 3000), a[2]],
            2 => [rng.range(-40, 680), a[1], rng.range(-40, 680)],
            _ => [
                a[0] + rng.range(-30, 30),
                a[1] + rng.range(-30, 30),
                a[2] + rng.range(-30, 30),
            ],
        };
        let q = match (i / 4) % 4 {
            0 => point(a, b),
            1 => CollisionTraceQuery::body(
                RoomPoint::from_array(a),
                RoomPoint::from_array(b),
                12,
                48,
            ),
            2 => CollisionTraceQuery::body(
                RoomPoint::from_array(a),
                RoomPoint::from_array(b),
                40,
                64,
            ),
            _ => {
                CollisionTraceQuery::body(RoomPoint::from_array(a), RoomPoint::from_array(b), 0, 30)
            }
        };
        queries.push(q);
    }
    let run = |built: &Built| -> Vec<CollisionTrace> {
        built.run(|w| queries.iter().map(|q| trace(w, *q)).collect())
    };
    let (x, y) = (run(&with), run(&without));
    for (i, (p, q)) in x.iter().zip(&y).enumerate() {
        assert_eq!(p, q, "query {i}: {:?}", queries[i]);
    }
    assert!(x.iter().filter(|t| t.fraction_q12 < 4096).count() > 300);
}

#[test]
fn point_traces_need_no_discrete_validation() {
    // The walk alone must agree with the walk plus the validation pass.
    for built in [property_world(), summary_world(true)] {
        built.run(|w| {
            let mut checked = *w;
            checked.validate = true;
            let mut rng = Lcg(4242);
            let mut hits = 0;
            for i in 0..40_000 {
                let a = [
                    rng.range(-40, 680),
                    rng.range(-80, 420),
                    rng.range(-40, 680),
                ];
                let b = match i % 3 {
                    0 => [
                        rng.range(-40, 680),
                        rng.range(-80, 420),
                        rng.range(-40, 680),
                    ],
                    1 => [
                        a[0] + rng.range(-120, 120),
                        a[1] + rng.range(-60, 60),
                        a[2] + rng.range(-120, 120),
                    ],
                    _ => [
                        a[0] + rng.range(-8, 8),
                        a[1] + rng.range(-8, 8),
                        a[2] + rng.range(-8, 8),
                    ],
                };
                let q = point(a, b);
                let (x, y) = (trace(w, q), trace(&checked, q));
                assert_eq!(x, y, "query {i}: {q:?}");
                hits += usize::from(x.fraction_q12 < 4096);
            }
            assert!(hits > 1000, "{hits}");
        });
    }
}

/// A 10 by 1 strip: floor 0 for cells 0..3, a tall plateau (200 runtime
/// units, far above the 40 the motor steps) from cell 3 on. `wall` adds the
/// cliff face the cook writes for it: a one-sided wall on the low cell's east
/// edge.
fn cliff_strip(wall: Option<u8>) -> Built {
    let mut room = Room::flat([0, 0, 0], 10, 1, 0);
    for cx in 3..10 {
        room.set_floor(cx, 0, 200);
    }
    if let Some(flags) = wall {
        room.wall_flagged(2, 0, edge::EAST, 0, 200, flags);
    }
    build(&[room])
}

#[test]
fn a_tall_riser_without_a_cliff_wall_lets_a_ray_pass_below_the_plateau() {
    // The H1 experiment of the natural terrain audit: the floor slab is thin,
    // so a ray (the camera arm, a shadow ray) through the rock below the high
    // floor meets nothing at all.
    let built = cliff_strip(None);
    built.run(|w| {
        let t = trace(w, point([20, 100, 40], [500, 100, 40]));
        assert_eq!(t.fraction_q12, 4096, "{t:?}");
    });
}

#[test]
fn a_front_only_cliff_wall_stops_a_ray_from_below_and_lets_one_leave_the_rock() {
    let built = cliff_strip(Some(WALL_FRONT_ONLY));
    built.run(|w| {
        // From the low side into the cliff: stopped at the face (x = 240).
        let t = trace(w, point([20, 100, 40], [500, 100, 40]));
        assert!(t.fraction_q12 < 4096, "{t:?}");
        assert!(t.end.x <= 240 && t.end.x > 230, "{t:?}");
        // A body below the plateau cannot walk into it either.
        let end = walk(w, RoomPoint::new(100, 0, 40), 4096, 0, 200);
        assert_eq!(end.y, 0, "{end:?}");
        assert!(end.x <= 240 - 12 && end.x > 240 - 12 - 10, "{end:?}");
        // From the plateau side the wall is not there: the ray travels on.
        let t = trace(w, point([500, 100, 40], [20, 100, 40]));
        assert_eq!(t.fraction_q12, 4096, "{t:?}");
    });
}

#[test]
fn a_ledge_can_be_walked_off_where_the_cliff_wall_is_one_sided() {
    let built = cliff_strip(Some(WALL_FRONT_ONLY));
    built.run(|w| {
        let end = walk(w, RoomPoint::new(400, 200, 40), -4096, 0, 200);
        assert!(end.x < 240, "must leave the plateau: {end:?}");
        assert_eq!(end.y, 0, "and land on the low floor: {end:?}");
    });
}

#[test]
fn back_only_walls_mirror_front_only_ones() {
    // The same strip with the plateau to the west: the diagonal-style flag.
    let mut room = Room::flat([0, 0, 0], 10, 1, 0);
    for cx in 0..3 {
        room.set_floor(cx, 0, 200);
    }
    // The low side is east of cell 2's east edge: that is the BACK of the
    // edge (the front is the cell interior), so the flag is back-only.
    room.wall_flagged(2, 0, edge::EAST, 0, 200, WALL_BACK_ONLY);
    let built = build(&[room]);
    built.run(|w| {
        let t = trace(w, point([500, 100, 40], [20, 100, 40]));
        assert!(t.fraction_q12 < 4096, "{t:?}");
        let t = trace(w, point([20, 100, 40], [500, 100, 40]));
        assert_eq!(t.fraction_q12, 4096, "{t:?}");
    });
}

/// One split-level cell in the middle of a flat strip: the NE half (first
/// triangle of the NW-SE split) is a 200 unit plateau, the SW half is level
/// with the rest. The cook's cliff wall for it is a back-only NW-SE wall.
fn diagonal_cliff() -> Built {
    let mut room = Room::flat([0, 0, 0], 3, 1, 0);
    {
        let c = room.cell(1, 0);
        c.floor = [200; 4];
        c.flags |= SECTOR_SPLIT_LEVEL;
    }
    room.splits.push((
        1,
        SplitB {
            floor_b: [0; 4],
            ceil_b: [0; 4],
        },
    ));
    room.wall_flagged(1, 0, edge::NWSE, 0, 200, WALL_BACK_ONLY);
    build(&[room])
}

#[test]
fn a_diagonal_cliff_stops_the_body_below_and_lets_the_one_on_the_plateau_walk_off() {
    let built = diagonal_cliff();
    built.run(|w| {
        // On the low (SW) half, walking east into the diagonal: stopped.
        let end = walk(w, RoomPoint::new(S + 5, 0, 70), 4096, 0, 200);
        assert_eq!(end.y, 0, "{end:?}");
        assert!(end.x < S + 70, "must not climb the cliff: {end:?}");
        // On the plateau, walking west off the diagonal: falls to the low floor.
        let end = walk(w, RoomPoint::new(S + 70, 200, 5), -4096, 0, 200);
        assert_eq!(end.y, 0, "{end:?}");
        assert!(end.x < S + 40, "{end:?}");
        // A ray from the low half into the rock under the plateau is stopped
        // at the face. (One starting inside the rock is stopped too, by the
        // split-level slab test: a ray never leaves rock sideways.)
        let t = trace(w, point([S + 10, 100, 70], [S + 70, 100, 10]));
        assert!(t.fraction_q12 < 4096, "{t:?}");
        let t = trace(w, point([S + 70, 100, 10], [S + 10, 100, 70]));
        assert!(t.fraction_q12 < 4096, "{t:?}");
    });
}
