//! Quadtree statistics on the cooked levels: how many leaves the exact
//! planar merge leaves, what that is in resident collision bytes, and how
//! many floor quads a colour-aware merge could remove.

use std::collections::HashMap;

use psx_grid::{
    Quad, QUAD_AXIS, QUAD_AXIS_NEG, QUAD_AXIS_SHIFT, QUAD_TRI, SECTOR_HAS_CEIL, SECTOR_HAS_FLOOR,
    SECTOR_SPLIT_LEVEL,
};

use crate::load::Loaded;
use crate::probe::{predict_uv_public, ru_to_fine_table};
use crate::tables::Room;

#[derive(Default, Clone, Copy)]
struct Leaves {
    by_level: [usize; 3],
    empty: [usize; 3],
}

/// Fine-unit corner grid of the floor (or ceiling) over the whole room:
/// per sector the four corners, `None` when the sector holds no surface.
fn corners(room: &Room, inv: &HashMap<i32, i32>, ceiling: bool) -> Vec<Option<[i32; 4]>> {
    room.sectors
        .iter()
        .map(|s| {
            let bit = if ceiling {
                SECTOR_HAS_CEIL
            } else {
                SECTOR_HAS_FLOOR
            };
            (s.flags & bit != 0).then(|| {
                let h = if ceiling { s.ceil } else { s.floor };
                [0, 1, 2, 3].map(|i| {
                    inv.get(&i32::from(h[i]))
                        .copied()
                        .unwrap_or(i32::from(h[i]))
                })
            })
        })
        .collect()
}

/// Whether the sectors of the `n` by `n` block at `(bx, bz)` form one leaf:
/// identical flag words, no extension, and one plane through every corner
/// (floor and ceiling each), or all empty.
fn block_leaf(
    room: &Room,
    floor: &[Option<[i32; 4]>],
    ceil: &[Option<[i32; 4]>],
    bx: usize,
    bz: usize,
    n: usize,
) -> Option<bool> {
    if bx + n > room.width || bz + n > room.depth {
        return None;
    }
    let first = room.sectors[bz * room.width + bx];
    let key = first.flags
        & !(psx_grid::SECTOR_FLAT_FLOOR
            | psx_grid::SECTOR_FLAT_CEIL
            | psx_grid::SECTOR_CLEAR
            | psx_grid::SECTOR_FLOOR_WALK_A
            | psx_grid::SECTOR_FLOOR_WALK_B);
    let mut all_empty = true;
    for dz in 0..n {
        for dx in 0..n {
            let idx = (bz + dz) * room.width + bx + dx;
            let s = room.sectors[idx];
            let k = s.flags
                & !(psx_grid::SECTOR_FLAT_FLOOR
                    | psx_grid::SECTOR_FLAT_CEIL
                    | psx_grid::SECTOR_CLEAR
                    | psx_grid::SECTOR_FLOOR_WALK_A
                    | psx_grid::SECTOR_FLOOR_WALK_B);
            if k != key || s.ext != psx_grid::NO_EXT || s.flags & SECTOR_SPLIT_LEVEL != 0 {
                return None;
            }
            if s.flags & (SECTOR_HAS_FLOOR | SECTOR_HAS_CEIL) != 0 {
                all_empty = false;
            }
        }
    }
    if all_empty {
        return Some(true);
    }
    for layer in [floor, ceil] {
        let Some(base) = layer[bz * room.width + bx] else {
            continue;
        };
        // Plane through the first sector: h(i, j) at sector corner (i, j) in
        // sector units, taken from corners NW(0,0) NE(1,0) SW(0,1).
        let (a, b, c) = (base[1] - base[0], base[3] - base[0], base[0]);
        if base[2] != base[1] + base[3] - base[0] {
            return None;
        }
        for dz in 0..n {
            for dx in 0..n {
                let h = layer[(bz + dz) * room.width + bx + dx]?;
                let (i, j) = (dx as i32, dz as i32);
                let want = |di: i32, dj: i32| c + a * (i + di) + b * (j + dj);
                if h[0] != want(0, 0)
                    || h[1] != want(1, 0)
                    || h[2] != want(1, 1)
                    || h[3] != want(0, 1)
                {
                    return None;
                }
            }
        }
    }
    Some(false)
}

fn count_leaves(
    room: &Room,
    inv: &HashMap<i32, i32>,
    origin: [i32; 2],
    world_aligned: bool,
) -> (Leaves, usize) {
    let floor = corners(room, inv, false);
    let ceil = corners(room, inv, true);
    let mut leaves = Leaves::default();
    let mut covered = vec![false; room.sectors.len()];
    let mut nodes = 0usize;
    for (li, n) in [4usize, 2, 1].into_iter().enumerate() {
        for bz in 0..room.depth {
            for bx in 0..room.width {
                if world_aligned {
                    if (origin[0] + bx as i32).rem_euclid(n as i32) != 0
                        || (origin[1] + bz as i32).rem_euclid(n as i32) != 0
                    {
                        continue;
                    }
                } else if bx % n != 0 || bz % n != 0 {
                    continue;
                }
                if (0..n).any(|dz| {
                    (0..n).any(|dx| {
                        bz + dz < room.depth
                            && bx + dx < room.width
                            && covered[(bz + dz) * room.width + bx + dx]
                    })
                }) {
                    continue;
                }
                let ok = if n == 1 {
                    Some(true)
                } else {
                    block_leaf(room, &floor, &ceil, bx, bz, n)
                };
                let leaf = if n == 1 { Some(true) } else { ok };
                if leaf.is_some() {
                    let empty = (0..n).all(|dz| {
                        (0..n).all(|dx| {
                            bz + dz >= room.depth
                                || bx + dx >= room.width
                                || room.sectors[(bz + dz) * room.width + bx + dx].flags
                                    & (SECTOR_HAS_FLOOR | SECTOR_HAS_CEIL)
                                    == 0
                        })
                    });
                    if n > 1 && block_leaf(room, &floor, &ceil, bx, bz, n).is_none() {
                        continue;
                    }
                    leaves.by_level[li] += 1;
                    if empty {
                        leaves.empty[li] += 1;
                    }
                    nodes += 1;
                    for dz in 0..n {
                        for dx in 0..n {
                            if bz + dz < room.depth && bx + dx < room.width {
                                covered[(bz + dz) * room.width + bx + dx] = true;
                            }
                        }
                    }
                }
            }
        }
    }
    (leaves, nodes)
}

pub fn run(levels: &[Loaded]) {
    println!("\n# Quadtree leaves on the reference levels (exact planar merge)\n");
    println!("level           sectors | world-aligned leaves (4x4/2x2/1x1, empties) | resident B dense -> leaves+index | room-aligned leaves");
    for level in levels {
        let lv = level.level();
        let s = lv.sector_units() as i32;
        let inv = ru_to_fine_table(s);
        for r in 0..level.rooms() {
            let room = Room::from_view(&level.room(r), s);
            let rec = &lv.rooms[r];
            let origin = [rec.origin[0] / s, rec.origin[2] / s];
            let (wl, _) = count_leaves(&room, &inv, origin, true);
            let (rl, _) = count_leaves(&room, &inv, origin, false);
            let total = |l: &Leaves| l.by_level.iter().sum::<usize>();
            let dense = room.sectors.len() * 20;
            let leafb = total(&wl) * 20 + room.sectors.len() * 2;
            let leafb_room = total(&rl) * 20 + room.sectors.len() * 2;
            println!(
                "{:<14} r{r} {:>5} | {:>4} = {:?} empty {:?} | {:>6} -> {:>6} ({:+.0}%) | {:>4} = {:?} -> {:>6}",
                level.name,
                room.sectors.len(),
                total(&wl),
                wl.by_level,
                wl.empty,
                dense,
                leafb,
                100.0 * (leafb as f64 / dense as f64 - 1.0),
                total(&rl),
                rl.by_level,
                leafb_room
            );
        }
    }
    println!("\n# Floor quads a colour-aware merge could remove (2x2 and 4x4 blocks, same material, coplanar, default UV, light within tolerance)\n");
    println!("level           floor quads | 2x2 blocks mergeable at tol 0 / 4 / 8 / 16 | 4x4 blocks at tol 0 / 8 / 16");
    for level in levels {
        let lv = level.level();
        let s = lv.sector_units() as i32;
        let inv = ru_to_fine_table(s);
        for r in 0..level.rooms() {
            let room = Room::from_view(&level.room(r), s);
            let doc = &level.world.rooms[r];
            let click = s / 4;
            // Per cell: material, uv ok, colours at [NW, NE, SE, SW] by position.
            struct Cell {
                mat: u8,
                uv_ok: bool,
                col: [u32; 4],
                h: [i32; 4],
            }
            let mut cells: Vec<Option<Cell>> = (0..room.sectors.len()).map(|_| None).collect();
            let mut nfloor = 0;
            for cz in 0..room.depth {
                for cx in 0..room.width {
                    let cell = cz * room.width + cx;
                    for qi in room.cell_quads(cell) {
                        let q = &room.quads[qi];
                        if q.flags & QUAD_TRI != 0
                            || q.flags & QUAD_AXIS == 0
                            || (q.flags >> QUAD_AXIS_SHIFT) & 3 != 1
                            || q.flags & QUAD_AXIS_NEG != 0
                        {
                            continue;
                        }
                        // Corner mapping by position.
                        let mut col = [0u32; 4];
                        let mut h = [0i32; 4];
                        let mut ok = true;
                        for k in 0..4 {
                            let p = room.pos(q.v[k]);
                            let dx = p[0] / click - cx as i32 * 4;
                            let dz = p[2] / click - cz as i32 * 4;
                            let slot = match (dx, dz) {
                                (0, 0) => 0,
                                (4, 0) => 1,
                                (4, 4) => 2,
                                (0, 4) => 3,
                                _ => {
                                    ok = false;
                                    break;
                                }
                            };
                            col[slot] = q.color[k];
                            h[slot] = inv.get(&p[1]).copied().unwrap_or(p[1]);
                        }
                        if ok {
                            nfloor += 1;
                            cells[cell] = Some(Cell {
                                mat: q.material,
                                uv_ok: predict_uv_public(
                                    &room,
                                    q,
                                    doc.origin,
                                    doc.y_offset,
                                    s,
                                    &inv,
                                ),
                                col,
                                h,
                            });
                        }
                    }
                }
            }
            let block_ok = |bx: usize, bz: usize, n: usize, tol: i32| -> bool {
                if bx + n > room.width || bz + n > room.depth {
                    return false;
                }
                let get = |dx: usize, dz: usize| cells[(bz + dz) * room.width + bx + dx].as_ref();
                let Some(first) = get(0, 0) else { return false };
                for dz in 0..n {
                    for dx in 0..n {
                        let Some(c) = get(dx, dz) else { return false };
                        if c.mat != first.mat || !c.uv_ok {
                            return false;
                        }
                    }
                }
                // Plane through the block's outer corners must hold every corner.
                let nw = get(0, 0).unwrap().h[0];
                let ne = get(n - 1, 0).unwrap().h[1];
                let se = get(n - 1, n - 1).unwrap().h[2];
                let sw = get(0, n - 1).unwrap().h[3];
                if nw + se != ne + sw {
                    return false;
                }
                let colour_at = |u: usize, v: usize| -> [i32; 3] {
                    // lattice point (u, v) in sector units inside the block
                    let (dx, dz, slot) = match (u == n, v == n) {
                        (false, false) => (u, v, 0),
                        (true, false) => (u - 1, v, 1),
                        (true, true) => (u - 1, v - 1, 2),
                        (false, true) => (u, v - 1, 3),
                    };
                    let c = get(dx, dz).unwrap().col[slot];
                    [
                        (c & 0xff) as i32,
                        ((c >> 8) & 0xff) as i32,
                        ((c >> 16) & 0xff) as i32,
                    ]
                };
                let hh = |u: usize, v: usize| -> i32 {
                    let (dx, dz, slot) = match (u == n, v == n) {
                        (false, false) => (u, v, 0),
                        (true, false) => (u - 1, v, 1),
                        (true, true) => (u - 1, v - 1, 2),
                        (false, true) => (u, v - 1, 3),
                    };
                    get(dx, dz).unwrap().h[slot]
                };
                let (c00, c10, c11, c01) = (
                    colour_at(0, 0),
                    colour_at(n, 0),
                    colour_at(n, n),
                    colour_at(0, n),
                );
                for v in 0..=n {
                    for u in 0..=n {
                        // plane height check
                        let (fu, fv) = (u as i64, v as i64);
                        let nn = n as i64;
                        let plane = i64::from(nw) * (nn - fu) * (nn - fv)
                            + i64::from(ne) * fu * (nn - fv)
                            + i64::from(se) * fu * fv
                            + i64::from(sw) * (nn - fu) * fv;
                        if plane != i64::from(hh(u, v)) * nn * nn {
                            return false;
                        }
                        let got = colour_at(u, v);
                        for ch in 0..3 {
                            let want = (i64::from(c00[ch]) * (nn - fu) * (nn - fv)
                                + i64::from(c10[ch]) * fu * (nn - fv)
                                + i64::from(c11[ch]) * fu * fv
                                + i64::from(c01[ch]) * (nn - fu) * fv)
                                as f64
                                / (nn * nn) as f64;
                            if (f64::from(got[ch]) - want).abs() > f64::from(tol) {
                                return false;
                            }
                        }
                    }
                }
                true
            };
            let count_blocks = |n: usize, tol: i32| -> usize {
                let mut used = vec![false; room.sectors.len()];
                let mut count = 0;
                for bz in 0..room.depth {
                    for bx in 0..room.width {
                        if bx % n != 0 || bz % n != 0 {
                            continue;
                        }
                        if block_ok(bx, bz, n, tol) {
                            count += 1;
                            for dz in 0..n {
                                for dx in 0..n {
                                    used[(bz + dz) * room.width + bx + dx] = true;
                                }
                            }
                        }
                    }
                }
                let _ = used;
                count
            };
            println!(
                "{:<14} r{r} {:>5} | {:>4} / {:>4} / {:>4} / {:>4} | {:>3} / {:>3} / {:>3}",
                level.name,
                nfloor,
                count_blocks(2, 0),
                count_blocks(2, 4),
                count_blocks(2, 8),
                count_blocks(2, 16),
                count_blocks(4, 0),
                count_blocks(4, 8),
                count_blocks(4, 16)
            );
        }
    }
    let _ = Quad::default();
}

/// Resident form candidates, in bytes per level, from table counts.
pub fn resident(levels: &[Loaded]) {
    println!("\n# Resident form (bytes): today / leaf collision / + 32 byte quads with a room colour palette\n");
    println!("level           today | collision leaves | +quad 32 B (palette) | saved");
    for level in levels {
        let lv = level.level();
        let s = lv.sector_units() as i32;
        let inv = ru_to_fine_table(s);
        let (mut today, mut leafform, mut tight) = (0usize, 0usize, 0usize);
        for r in 0..level.rooms() {
            let room = Room::from_view(&level.room(r), s);
            let rec = &lv.rooms[r];
            let origin = [rec.origin[0] / s, rec.origin[2] / s];
            let (wl, _) = count_leaves(&room, &inv, origin, true);
            let leaves: usize = wl.by_level.iter().sum();
            let raw = level.room_bytes(r);
            let col_dense = room.sectors.len() * 20;
            let col_leaf = leaves * 20 + room.sectors.len() * 2;
            let palette: std::collections::HashSet<u32> =
                room.quads.iter().flat_map(|q| q.color).collect();
            let quads40 = room.quads.len() * 40;
            let quads32 = room.quads.len() * 32 + palette.len() * 4;
            today += raw;
            let l = raw - col_dense + col_leaf.min(col_dense);
            leafform += l;
            tight += l - quads40 + quads32;
        }
        println!(
            "{:<14} {:>7} | {:>7} ({:+.1}%) | {:>7} ({:+.1}%)",
            level.name,
            today,
            leafform,
            100.0 * (leafform as f64 / today as f64 - 1.0),
            tight,
            100.0 * (tight as f64 / today as f64 - 1.0)
        );
    }
}
