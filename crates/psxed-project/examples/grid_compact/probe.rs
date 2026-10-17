//! Stage 1b: what the current cooked tables carry that a decoder could
//! regenerate, and how often. Every figure is a count over the cooked tables.

use std::collections::{HashMap, HashSet};

use psx_grid::{
    Quad, QUAD_AXIS, QUAD_AXIS_NEG, QUAD_AXIS_SHIFT, QUAD_DOUBLE, QUAD_LOD_COVERED, QUAD_PLANE,
    QUAD_TRI, SECTOR_CEIL_NESW, SECTOR_FLOOR_NESW, SECTOR_HAS_CEIL, SECTOR_HAS_FLOOR,
    SECTOR_SPLIT_LEVEL,
};

use crate::load::Loaded;
use crate::tables::Room;

pub fn probe(levels: &[Loaded]) {
    for level in levels {
        let lv = level.level();
        let s = lv.sector_units() as i32;
        println!("== {} (S={s}) ==", level.name);
        for r in 0..level.rooms() {
            let room = Room::from_view(&level.room(r), s);
            println!("-- room {r}: {}x{}", room.width, room.depth);
            classes(&room);
            colours(&room);
            lattice(&room);
            sectors(&room);
            misc(&room);
        }
    }
}

fn normal_of(room: &Room, q: &Quad) -> [i32; 3] {
    let ring = room.ring(q);
    for i in 1..ring.len() - 1 {
        let a = sub(ring[i], ring[0]);
        let b = sub(ring[i + 1], ring[0]);
        let n = cross(a, b);
        if n != [0; 3] {
            return n;
        }
    }
    [0; 3]
}

fn sub(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn classes(room: &Room) {
    let (mut floor, mut ceil, mut wall, mut other) = (0, 0, 0, 0);
    let (mut tri, mut axis, mut double, mut repeat, mut plane, mut covered) = (0, 0, 0, 0, 0, 0);
    let mut axis_hist = [0usize; 6];
    for q in &room.quads {
        let n = normal_of(room, q);
        let ay = n[1].abs();
        let horiz = i64::from(n[0]).pow(2) + i64::from(n[2]).pow(2);
        if n[1] > 0 && i64::from(ay).pow(2) >= horiz {
            floor += 1;
        } else if n[1] < 0 && i64::from(ay).pow(2) >= horiz {
            ceil += 1;
        } else if ay == 0 || i64::from(ay).pow(2) * 4 < horiz {
            wall += 1;
        } else {
            other += 1;
        }
        if q.flags & QUAD_TRI != 0 {
            tri += 1;
        }
        if q.flags & QUAD_AXIS != 0 {
            axis += 1;
            let id = usize::from((q.flags >> QUAD_AXIS_SHIFT) & 3);
            axis_hist[id * 2 + usize::from(q.flags & QUAD_AXIS_NEG != 0)] += 1;
        }
        if q.flags & QUAD_DOUBLE != 0 {
            double += 1;
        }
        if q.flags2 & psx_grid::QUAD2_REPEAT != 0 {
            repeat += 1;
        }
        if q.flags & QUAD_PLANE != 0 {
            plane += 1;
        }
        if q.flags & QUAD_LOD_COVERED != 0 {
            covered += 1;
        }
    }
    let n = room.quads.len();
    println!(
        "  quads {n}: floor-like {floor}, ceiling-like {ceil}, wall-like {wall}, slanted {other}; \
         tri {tri}, axis-aligned {axis} (x+ {} x- {} y+ {} y- {} z+ {} z-{}), double {double}, repeat {repeat}, plane {plane}, lod-covered {covered}",
        axis_hist[0], axis_hist[1], axis_hist[2], axis_hist[3], axis_hist[4], axis_hist[5]
    );
}

fn colours(room: &Room) {
    let n = room.quads.len();
    let mut distinct: HashSet<u32> = HashSet::new();
    let mut flat_quads = 0;
    let mut top_byte_used = 0;
    let mut by_key: HashMap<(u16, [i8; 3]), HashSet<u32>> = HashMap::new();
    let mut corner_uses = 0;
    for q in &room.quads {
        let count = if q.flags & QUAD_TRI != 0 { 3 } else { 4 };
        if (1..count).all(|k| q.color[k] == q.color[0]) {
            flat_quads += 1;
        }
        for k in 0..count {
            corner_uses += 1;
            distinct.insert(q.color[k]);
            if q.color[k] >> 24 != 0 {
                top_byte_used += 1;
            }
            by_key.entry((q.v[k], q.n)).or_default().insert(q.color[k]);
        }
    }
    let conflicts = by_key.values().filter(|set| set.len() > 1).count();
    let rgb555_lossless = room
        .quads
        .iter()
        .flat_map(|q| q.color)
        .filter(|c| (c & 0x00_07_07_07) != 0)
        .count();
    println!(
        "  colours: {} distinct of {corner_uses} corner uses; quads with 4 equal corners {flat_quads}/{n}; \
         (vertex,normal) keys {} (sharing {:.2} uses/key), keys with >1 colour {conflicts}; \
         corners not representable in 555 {rgb555_lossless}; top byte used {top_byte_used}",
        distinct.len(),
        by_key.len(),
        corner_uses as f64 / by_key.len().max(1) as f64
    );
}

fn lattice(room: &Room) {
    let s = room.s;
    let click = s / 4;
    let mut x_off = 0;
    let mut z_off = 0;
    let mut y_off = 0;
    let (mut ymin, mut ymax) = (i32::MAX, i32::MIN);
    for v in &room.vertices {
        if i32::from(v.x) % click != 0 {
            x_off += 1;
        }
        if i32::from(v.z) % click != 0 {
            z_off += 1;
        }
        if i32::from(v.y) % (click / 4).max(1) != 0 {
            y_off += 1;
        }
        ymin = ymin.min(i32::from(v.y));
        ymax = ymax.max(i32::from(v.y));
    }
    let pad = room.vertices.iter().filter(|v| v.pad != 0).count();
    println!(
        "  vertices {}: off-click x {x_off} z {z_off}, off-fine y {y_off}, y range {ymin}..{ymax}, pad nonzero {pad}",
        room.vertices.len()
    );
}

fn sectors(room: &Room) {
    let mut flag_hist: HashMap<u16, usize> = HashMap::new();
    let mut flat_floor = 0;
    let mut has_floor = 0;
    let mut has_ceil = 0;
    let mut split_level = 0;
    let mut nesw = 0;
    let mut planar_floor = 0;
    for sec in &room.sectors {
        *flag_hist
            .entry(
                sec.flags
                    & !(psx_grid::SECTOR_FLAT_FLOOR
                        | psx_grid::SECTOR_FLAT_CEIL
                        | psx_grid::SECTOR_CLEAR),
            )
            .or_default() += 1;
        if sec.flags & SECTOR_HAS_FLOOR != 0 {
            has_floor += 1;
            if sec.floor.iter().all(|h| *h == sec.floor[0]) {
                flat_floor += 1;
            }
            if sec.floor[0] + sec.floor[2] == sec.floor[1] + sec.floor[3] {
                planar_floor += 1;
            }
        }
        if sec.flags & SECTOR_HAS_CEIL != 0 {
            has_ceil += 1;
        }
        if sec.flags & SECTOR_SPLIT_LEVEL != 0 {
            split_level += 1;
        }
        if sec.flags & SECTOR_FLOOR_NESW != 0 {
            nesw += 1;
        }
        let _ = SECTOR_CEIL_NESW;
    }
    println!(
        "  sectors {}: floor {has_floor} (flat {flat_floor}, planar {planar_floor}), ceiling {has_ceil}, split-level {split_level}, nesw {nesw}, distinct flag words {}",
        room.sectors.len(),
        flag_hist.len()
    );
}

fn misc(room: &Room) {
    let mut prefix_ok = true;
    let mut sum = 0usize;
    for c in &room.cells {
        if usize::from(c.first) != sum {
            prefix_ok = false;
        }
        sum += usize::from(c.count);
    }
    let reserved_walls = room.walls.iter().filter(|w| w.reserved != 0).count();
    println!(
        "  cells: first is the running sum of counts: {prefix_ok} (plane quads follow: total counted {sum} of {}); walls with reserved set {reserved_walls}",
        room.quads.len()
    );
}

// ---------------------------------------------------------------------------
// Probe 2: heights in fine units, UV prediction, wall coverage.
// ---------------------------------------------------------------------------

/// `round_half_away_from_zero(k * s / 16)`, the cook's mapping of a height of
/// `k` fine steps (64 grid units) to runtime units.
pub fn fine_to_ru(k: i32, s: i32) -> i32 {
    let num = i64::from(k) * i64::from(s);
    let r = if num >= 0 {
        (num + 8) / 16
    } else {
        -((-num + 8) / 16)
    };
    r as i32
}

pub fn ru_to_fine_table(s: i32) -> HashMap<i32, i32> {
    let mut m = HashMap::new();
    for k in -4096..=4096 {
        m.entry(fine_to_ru(k, s)).or_insert(k);
    }
    m
}

pub fn probe2(levels: &[Loaded]) {
    for level in levels {
        let lv = level.level();
        let s = lv.sector_units() as i32;
        let inv = ru_to_fine_table(s);
        println!("== {} (S={s}) ==", level.name);
        for r in 0..level.rooms() {
            let room = Room::from_view(&level.room(r), s);
            let doc = &level.world.rooms[r];
            // 1. heights representable in fine steps.
            let total = room.vertices.len();
            let bad = room
                .vertices
                .iter()
                .filter(|v| !inv.contains_key(&i32::from(v.y)))
                .count();
            let sec_bad = room
                .sectors
                .iter()
                .flat_map(|sec| {
                    let mut v = Vec::new();
                    if sec.flags & SECTOR_HAS_FLOOR != 0 {
                        v.extend(sec.floor);
                    }
                    if sec.flags & SECTOR_HAS_CEIL != 0 {
                        v.extend(sec.ceil);
                    }
                    v
                })
                .filter(|h| !inv.contains_key(&i32::from(*h)))
                .count();
            println!(
                "-- room {r}: vertex heights not on the fine grid {bad}/{total}, sector corner heights {sec_bad}"
            );
            // 2. UV prediction.
            let mut hit = 0;
            let mut miss = 0;
            let mut miss_by_class: HashMap<&'static str, usize> = HashMap::new();
            let mut hit_by_class: HashMap<&'static str, usize> = HashMap::new();
            for q in &room.quads {
                let class = if q.flags & QUAD_AXIS != 0 {
                    match (q.flags >> QUAD_AXIS_SHIFT) & 3 {
                        1 => "horizontal",
                        _ => "wall",
                    }
                } else {
                    "slanted"
                };
                if predict_uv(&room, q, doc.origin, doc.y_offset, s, &inv) {
                    hit += 1;
                    *hit_by_class.entry(class).or_default() += 1;
                } else {
                    miss += 1;
                    *miss_by_class.entry(class).or_default() += 1;
                }
            }
            println!(
                "   uv predicted exactly from vertex positions: {hit}/{} (hit {:?}, miss {:?})",
                hit + miss,
                hit_by_class,
                miss_by_class
            );
            walls(&room);
        }
    }
}

/// Texel of a point for a quad: world position times 32 per sector.
fn predict_uv(
    room: &Room,
    q: &Quad,
    origin: [i32; 2],
    y_offset: i32,
    s: i32,
    inv: &HashMap<i32, i32>,
) -> bool {
    let n = if q.flags & QUAD_TRI != 0 { 3 } else { 4 };
    let ring = room.ring(q);
    let stored: Vec<[i32; 2]> = (0..n)
        .map(|k| [i32::from(q.uv[k] & 0xff), i32::from(q.uv[k] >> 8)])
        .collect();
    // Candidate (axes, sign) pairs: floor (x,z), wall along x (u=x or -x, v=-y), wall along z.
    let world_x = |p: &[i32; 3]| -> f64 {
        f64::from(origin[0]) * 1024.0 + f64::from(p[0]) * 1024.0 / f64::from(s)
    };
    let world_z = |p: &[i32; 3]| -> f64 {
        f64::from(origin[1]) * 1024.0 + f64::from(p[2]) * 1024.0 / f64::from(s)
    };
    let world_y = |p: &[i32; 3]| -> Option<f64> {
        inv.get(&p[1])
            .map(|k| f64::from(y_offset) + f64::from(*k) * 64.0)
    };
    let candidates: Vec<Vec<[f64; 2]>> = {
        let mut c = Vec::new();
        c.push(
            ring.iter()
                .map(|p| [world_x(p) / 32.0, world_z(p) / 32.0])
                .collect(),
        );
        if ring
            .iter()
            .map(&world_y)
            .collect::<Option<Vec<_>>>()
            .is_some()
        {
            for su in [1.0, -1.0] {
                c.push(
                    ring.iter()
                        .map(|p| [su * world_x(p) / 32.0, -world_y(p).unwrap() / 32.0])
                        .collect(),
                );
                c.push(
                    ring.iter()
                        .map(|p| [su * world_z(p) / 32.0, -world_y(p).unwrap() / 32.0])
                        .collect(),
                );
            }
        }
        c
    };
    let repeat = q.flags2 & psx_grid::QUAD2_REPEAT != 0;
    'cand: for texels in candidates {
        let mut uvs = texels.clone();
        for axis in 0..2 {
            let min = uvs.iter().map(|t| t[axis]).fold(f64::MAX, f64::min);
            let max = uvs.iter().map(|t| t[axis]).fold(f64::MIN, f64::max);
            let shift = (min / 64.0).floor() * 64.0;
            let slack = 0.0001f64;
            let touched = ((((max - shift - slack) / 64.0).ceil()) as i64).max(1);
            let limit = if repeat { 4 } else { 1 };
            let tiles = touched.min(limit);
            let top = tiles as f64 * 64.0 - 1.0;
            for t in &mut uvs {
                t[axis] = (t[axis] - shift).clamp(0.0, top);
            }
        }
        for k in 0..n {
            let got = [uvs[k][0].round() as i32, uvs[k][1].round() as i32];
            if got != stored[k] {
                continue 'cand;
            }
        }
        return true;
    }
    false
}

fn walls(room: &Room) {
    // Wall coverage by the riser and closure strips, by area.
    let w = room.width as i32;
    let d = room.depth as i32;
    let s = room.s;
    let mut quads_in_walls = 0usize;
    let mut riser_quads = 0usize;
    let mut closure_quads = 0usize;
    let mut other_quads = 0usize;
    for cz in 0..d {
        for cx in 0..w {
            let cell = (cz * w + cx) as usize;
            let sec = &room.sectors[cell];
            // Collect vertical quads of this cell by edge.
            let mut per_edge: [Vec<&Quad>; 4] = Default::default();
            for qi in room.cell_quads(cell) {
                let q = &room.quads[qi];
                let ring = room.ring(q);
                let n = normal_of(room, q);
                if n[1] != 0 {
                    continue;
                }
                // On which cell edge do all corners lie?
                let x0 = cx * s;
                let z0 = cz * s;
                let on = |f: &dyn Fn(&[i32; 3]) -> bool| ring.iter().all(f);
                let e = if on(&|p| p[2] == z0) {
                    Some(0)
                } else if on(&|p| p[0] == x0 + s) {
                    Some(1)
                } else if on(&|p| p[2] == z0 + s) {
                    Some(2)
                } else if on(&|p| p[0] == x0) {
                    Some(3)
                } else {
                    None
                };
                quads_in_walls += 1;
                match e {
                    Some(e) => per_edge[e].push(q),
                    None => other_quads += 1,
                }
            }
            for e in 0..4 {
                if per_edge[e].is_empty() {
                    continue;
                }
                let cnt = per_edge[e].len();
                // Edge endpoints (a -> b) as corner indices of [NW, NE, SE, SW].
                let (ia, ib) = match e {
                    0 => (0, 1),
                    1 => (1, 2),
                    2 => (2, 3),
                    _ => (3, 0),
                };
                let own = |i: usize| i32::from(sec.floor[i]);
                // Neighbour across the edge, its matching corner indices.
                let (nx, nz, na, nb) = match e {
                    0 => (cx, cz - 1, 3, 2),
                    1 => (cx + 1, cz, 0, 3),
                    2 => (cx, cz + 1, 1, 0),
                    _ => (cx - 1, cz, 2, 1),
                };
                // Walking a->b on this cell, the neighbour sees b->a: matching
                // corner of a is the neighbour's corner at the same point.
                let inside = nx >= 0 && nz >= 0 && nx < w && nz < d;
                let ncell = if inside {
                    Some(&room.sectors[(nz * w + nx) as usize])
                } else {
                    None
                };
                let strip_area = |lo: (i32, i32), hi: (i32, i32)| -> i64 {
                    // doubled area between profiles lo(a,b) and hi(a,b)
                    i64::from(s) * i64::from((hi.0 - lo.0) + (hi.1 - lo.1))
                };
                let quads_area: i64 = per_edge[e]
                    .iter()
                    .map(|q| {
                        let ring = room.ring(q);
                        let coord = |p: &[i32; 3]| if e % 2 == 0 { p[0] } else { p[2] };
                        let mut sum = 0i64;
                        for k in 0..ring.len() {
                            let a = &ring[k];
                            let b = &ring[(k + 1) % ring.len()];
                            sum += i64::from(coord(a)) * i64::from(b[1])
                                - i64::from(coord(b)) * i64::from(a[1]);
                        }
                        sum.abs()
                    })
                    .sum();
                let mut kind = "other";
                if let Some(nc) = ncell {
                    if sec.flags & SECTOR_HAS_FLOOR != 0 && nc.flags & SECTOR_HAS_FLOOR != 0 {
                        let mine = (own(ia), own(ib));
                        // neighbour profile along the same a->b direction
                        let theirs = (i32::from(nc.floor[na]), i32::from(nc.floor[nb]));
                        let (lo, hi) = if mine.0 + mine.1 >= theirs.0 + theirs.1 {
                            (theirs, mine)
                        } else {
                            (mine, theirs)
                        };
                        if lo.0 <= hi.0 && lo.1 <= hi.1 && strip_area(lo, hi) == quads_area {
                            kind = "riser";
                        }
                    }
                }
                if kind == "other"
                    && sec.flags & SECTOR_HAS_FLOOR != 0
                    && sec.flags & SECTOR_HAS_CEIL != 0
                {
                    let lo = (own(ia), own(ib));
                    let hi = (i32::from(sec.ceil[ia]), i32::from(sec.ceil[ib]));
                    if strip_area(lo, hi) == quads_area {
                        kind = "closure";
                    }
                }
                match kind {
                    "riser" => riser_quads += cnt,
                    "closure" => closure_quads += cnt,
                    _ => other_quads += cnt,
                }
            }
        }
    }
    println!(
        "   cardinal wall quads {quads_in_walls}: riser-strip covered {riser_quads}, closure-strip covered {closure_quads}, other {other_quads}"
    );
}

/// Print the wall quads of a few cells with their surroundings.
pub fn dump_walls(level: &Loaded, limit: usize) {
    let lv = level.level();
    let s = lv.sector_units() as i32;
    let inv = ru_to_fine_table(s);
    let room = Room::from_view(&level.room(0), s);
    let k = |y: i32| inv.get(&y).copied().unwrap_or(i32::MIN);
    let mut shown = 0;
    for cz in 0..room.depth as i32 {
        for cx in 0..room.width as i32 {
            let cell = cz as usize * room.width + cx as usize;
            let sec = &room.sectors[cell];
            let walls: Vec<&Quad> = room
                .cell_quads(cell)
                .map(|i| &room.quads[i])
                .filter(|q| normal_of(&room, q)[1] == 0)
                .collect();
            if walls.is_empty() {
                continue;
            }
            shown += 1;
            if shown > limit {
                return;
            }
            println!(
                "cell ({cx},{cz}) floor {:?} ceil {:?} flags {:#x}",
                sec.floor.map(|h| k(i32::from(h))),
                sec.ceil.map(|h| k(i32::from(h))),
                sec.flags
            );
            for q in walls {
                let ring = room.ring(q);
                println!(
                    "    n={:?} ring(x/z cells, fine y)={:?}",
                    q.n,
                    ring.iter()
                        .map(|p| (p[0] as f64 / s as f64, p[2] as f64 / s as f64, k(p[1])))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}

pub fn predict_uv_public(
    room: &Room,
    q: &Quad,
    origin: [i32; 2],
    y_offset: i32,
    s: i32,
    inv: &HashMap<i32, i32>,
) -> bool {
    predict_uv(room, q, origin, y_offset, s, inv)
}
