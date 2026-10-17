//! The "as few bits as possible" rule, measured: for every field of every
//! cooked record, its current width, the range actually present in a room,
//! the width that range needs, and the bits a per-room dynamic width saves.

use std::collections::HashSet;

use psx_grid::{
    QUAD_TRI, SECTOR_CLEAR, SECTOR_FLAT_CEIL, SECTOR_FLAT_FLOOR, SECTOR_HAS_CEIL, SECTOR_HAS_FLOOR,
};

use crate::load::Loaded;
use crate::probe::ru_to_fine_table;
use crate::tables::Room;

/// Bits for an unsigned count of distinct values `n` (0 or 1 value: 0 bits).
pub fn bits_for_count(n: u64) -> u32 {
    if n <= 1 {
        0
    } else {
        64 - (n - 1).leading_zeros()
    }
}

/// Bits for the closed range `lo..=hi`.
pub fn bits_for_range(lo: i64, hi: i64) -> u32 {
    bits_for_count((hi - lo + 1) as u64)
}

struct Column {
    table: &'static str,
    name: &'static str,
    current: u32,
    values: Vec<i64>,
    /// Override of the measured width (palettes, masks).
    min_bits: Option<u32>,
    note: &'static str,
}

impl Column {
    fn new(table: &'static str, name: &'static str, current: u32, values: Vec<i64>) -> Self {
        Self {
            table,
            name,
            current,
            values,
            min_bits: None,
            note: "",
        }
    }
    fn bits(&self) -> u32 {
        if let Some(b) = self.min_bits {
            return b;
        }
        match (self.values.iter().min(), self.values.iter().max()) {
            (Some(lo), Some(hi)) => bits_for_range(*lo, *hi),
            _ => 0,
        }
    }
    fn range(&self) -> (i64, i64) {
        (
            self.values.iter().copied().min().unwrap_or(0),
            self.values.iter().copied().max().unwrap_or(0),
        )
    }
}

fn mask_bits(values: &[i64]) -> u32 {
    let or = values.iter().fold(0i64, |a, b| a | b);
    or.count_ones()
}

fn palette_bits(values: &[i64]) -> u32 {
    let set: HashSet<i64> = values.iter().copied().collect();
    bits_for_count(set.len() as u64)
}

fn columns(room: &Room, inv: &std::collections::HashMap<i32, i32>) -> Vec<Column> {
    let fine = |y: i16| i64::from(inv.get(&i32::from(y)).copied().unwrap_or(i32::from(y)));
    let s4 = (room.s / 4).max(1);
    let mut c = Vec::new();
    // Sector.
    let mut floor = Vec::new();
    let mut ceil = Vec::new();
    let mut flags = Vec::new();
    let mut ext = Vec::new();
    for sec in &room.sectors {
        if sec.flags & SECTOR_HAS_FLOOR != 0 {
            floor.extend(sec.floor.iter().map(|h| fine(*h)));
        }
        if sec.flags & SECTOR_HAS_CEIL != 0 {
            ceil.extend(sec.ceil.iter().map(|h| fine(*h)));
        }
        flags.push(i64::from(
            sec.flags & !(SECTOR_FLAT_FLOOR | SECTOR_FLAT_CEIL | SECTOR_CLEAR),
        ));
        if sec.ext != psx_grid::NO_EXT {
            ext.push(i64::from(sec.ext));
        }
    }
    let mut col = Column::new("Sector", "floor[4] (fine units)", 16, floor);
    col.note = "x4 per sector with a floor";
    c.push(col);
    c.push(Column::new("Sector", "ceil[4] (fine units)", 16, ceil));
    let mut col = Column::new("Sector", "flags (palette index)", 16, flags.clone());
    col.min_bits = Some(palette_bits(&flags).min(mask_bits(&flags)));
    col.note = "min(palette index, used bit count); 3 summary bits derived";
    c.push(col);
    let mut col = Column::new("Sector", "ext index", 16, vec![0, room.exts.len() as i64]);
    col.min_bits = Some(if room.exts.is_empty() {
        0
    } else {
        bits_for_count(room.exts.len() as u64 + 1)
    });
    let _ = ext;
    c.push(col);
    // SectorExt.
    let n = room.exts.len();
    c.push(Column::new(
        "SectorExt",
        "wall_first",
        16,
        room.exts.iter().map(|e| i64::from(e.wall_first)).collect(),
    ));
    c.push(Column::new(
        "SectorExt",
        "wall_count",
        8,
        room.exts.iter().map(|e| i64::from(e.wall_count)).collect(),
    ));
    c.push(Column::new(
        "SectorExt",
        "plane_count",
        8,
        room.exts.iter().map(|e| i64::from(e.plane_count)).collect(),
    ));
    c.push(Column::new(
        "SectorExt",
        "plane_first",
        16,
        room.exts.iter().map(|e| i64::from(e.plane_first)).collect(),
    ));
    let mut col = Column::new(
        "SectorExt",
        "splitb",
        16,
        room.exts.iter().map(|e| i64::from(e.splitb)).collect(),
    );
    col.min_bits = Some(if room.splitbs.is_empty() {
        0
    } else {
        bits_for_count(room.splitbs.len() as u64 + 1)
    });
    c.push(col);
    let _ = n;
    // ColWall.
    c.push(Column::new(
        "ColWall",
        "edge",
        8,
        room.walls.iter().map(|w| i64::from(w.edge)).collect(),
    ));
    let mut col = Column::new(
        "ColWall",
        "flags",
        8,
        room.walls.iter().map(|w| i64::from(w.flags)).collect(),
    );
    col.min_bits = Some(mask_bits(&col.values));
    c.push(col);
    c.push(Column::new(
        "ColWall",
        "y[4] (fine units)",
        16,
        room.walls.iter().flat_map(|w| w.y.map(fine)).collect(),
    ));
    c.push(Column::new(
        "ColWall",
        "reserved",
        16,
        room.walls.iter().map(|w| i64::from(w.reserved)).collect(),
    ));
    // SplitB.
    c.push(Column::new(
        "SplitB",
        "floor_b/ceil_b[8] (fine)",
        16,
        room.splitbs
            .iter()
            .flat_map(|b| b.floor_b.into_iter().chain(b.ceil_b))
            .map(fine)
            .collect(),
    ));
    // Vtx.
    c.push(Column::new(
        "Vtx",
        "x (click lattice)",
        16,
        room.vertices
            .iter()
            .map(|v| i64::from(v.x) / i64::from(s4))
            .collect(),
    ));
    c.push(Column::new(
        "Vtx",
        "y (fine units)",
        16,
        room.vertices.iter().map(|v| fine(v.y)).collect(),
    ));
    c.push(Column::new(
        "Vtx",
        "z (click lattice)",
        16,
        room.vertices
            .iter()
            .map(|v| i64::from(v.z) / i64::from(s4))
            .collect(),
    ));
    c.push(Column::new(
        "Vtx",
        "pad",
        16,
        room.vertices.iter().map(|v| i64::from(v.pad)).collect(),
    ));
    // Quad.
    let nv = room.vertices.len() as u64;
    let quads: Vec<&psx_grid::Quad> = room.quads.iter().collect();
    for k in 0..4 {
        let _ = k;
    }
    let mut col = Column::new(
        "Quad",
        "v[4] (vertex index)",
        16,
        quads
            .iter()
            .flat_map(|q| {
                let n = if q.flags & QUAD_TRI != 0 { 3 } else { 4 };
                q.v[..n].iter().map(|v| i64::from(*v)).collect::<Vec<_>>()
            })
            .collect(),
    );
    col.min_bits = Some(bits_for_count(nv));
    c.push(col);
    let colours: Vec<i64> = quads
        .iter()
        .flat_map(|q| {
            let n = if q.flags & QUAD_TRI != 0 { 3 } else { 4 };
            q.color[..n]
                .iter()
                .map(|v| i64::from(*v))
                .collect::<Vec<_>>()
        })
        .collect();
    let mut col = Column::new("Quad", "color[4] (room palette index)", 32, colours.clone());
    col.min_bits = Some(palette_bits(&colours));
    col.note = "palette bytes extra: 3 B per entry";
    c.push(col);
    let u: Vec<i64> = quads
        .iter()
        .flat_map(|q| q.uv.map(|t| i64::from(t & 0xff)))
        .collect();
    let v: Vec<i64> = quads
        .iter()
        .flat_map(|q| q.uv.map(|t| i64::from(t >> 8)))
        .collect();
    c.push(Column::new("Quad", "uv u[4]", 8, u));
    c.push(Column::new("Quad", "uv v[4]", 8, v));
    c.push(Column::new(
        "Quad",
        "d",
        16,
        quads.iter().map(|q| i64::from(q.d)).collect(),
    ));
    c.push(Column::new(
        "Quad",
        "n[3]",
        8,
        quads.iter().flat_map(|q| q.n.map(i64::from)).collect(),
    ));
    let mut col = Column::new(
        "Quad",
        "material (room palette)",
        8,
        quads.iter().map(|q| i64::from(q.material)).collect(),
    );
    col.min_bits = Some(palette_bits(&col.values));
    c.push(col);
    let mut col = Column::new(
        "Quad",
        "flags",
        8,
        quads.iter().map(|q| i64::from(q.flags)).collect(),
    );
    col.min_bits = Some(mask_bits(&col.values));
    c.push(col);
    let mut col = Column::new(
        "Quad",
        "flags2",
        8,
        quads.iter().map(|q| i64::from(q.flags2)).collect(),
    );
    col.min_bits = Some(mask_bits(&col.values));
    c.push(col);
    // RCell.
    let mut col = Column::new(
        "RCell",
        "first",
        16,
        room.cells.iter().map(|x| i64::from(x.first)).collect(),
    );
    col.min_bits = Some(bits_for_count(room.quads.len() as u64 + 1));
    c.push(col);
    c.push(Column::new(
        "RCell",
        "count",
        8,
        room.cells.iter().map(|x| i64::from(x.count)).collect(),
    ));
    let mut col = Column::new(
        "RCell",
        "flags",
        8,
        room.cells.iter().map(|x| i64::from(x.flags)).collect(),
    );
    col.min_bits = Some(mask_bits(&col.values));
    c.push(col);
    c.push(Column::new(
        "RCell",
        "min_y,max_y (fine)",
        16,
        room.cells
            .iter()
            .flat_map(|x| {
                if x.count == 0 {
                    vec![]
                } else {
                    vec![fine(x.min_y), fine(x.max_y)]
                }
            })
            .collect(),
    ));
    c
}

/// How a field is chosen under the rule, per field name: (chosen bits per
/// value rule, how it is derived, R3000 cost of reading it back).
fn rule(table: &str, name: &str) -> (&'static str, &'static str) {
    match (table, name) {
        ("Sector", n) if n.starts_with("floor") => ("range", "4 fields from one packed word run: 2 ALU each (srl+andi), +1 add of the room base"),
        ("Sector", n) if n.starts_with("ceil") => ("range", "as floor; absent (0 bits) when HAS_CEIL is clear"),
        ("Sector", "flags (palette index)") => ("palette", "1 table load + 1 mask, or the bits unpacked in place; 3 summary bits recomputed at install"),
        ("Sector", "ext index") => ("implicit", "running count of sectors with walls/planes/split: 0 bits, derived at install"),
        ("SectorExt", "wall_first") => ("implicit", "prefix sum of wall_count, derived at install"),
        ("SectorExt", "wall_count") => ("range", "2 ALU"),
        ("SectorExt", "plane_count") | ("SectorExt", "plane_first") => ("range", "0 bits when the room has no planes"),
        ("SectorExt", "splitb") => ("implicit", "running count over sectors with SPLIT_LEVEL"),
        ("ColWall", "edge") => ("range", "2 ALU"),
        ("ColWall", "flags") => ("range", "0 bits when constant"),
        ("ColWall", n) if n.starts_with("y") => ("range", "predicted from the sector (see codec), else 2 ALU + base"),
        ("ColWall", "reserved") => ("implicit", "dropped"),
        ("SplitB", _) => ("range", "2 ALU + base"),
        ("Vtx", "pad") => ("implicit", "dropped on disc; resident keeps it (GTE LWC2 needs 8 B)"),
        ("Vtx", _) => ("range", "disc: 2 ALU; resident: rebuilt as i16 x*click (1 mul or shift)"),
        ("Quad", n) if n.starts_with("v[") => ("range", "disc: implicit via welding (new vertex = next id); resident: 16 bit"),
        ("Quad", n) if n.starts_with("color") => ("palette", "palette table load per corner (1 lw, 7 cycles), or shared per (vertex,normal)"),
        ("Quad", "uv u[4]") | ("Quad", "uv v[4]") => ("derive", "predicted from vertex positions where the mapping is the default world one; stored otherwise"),
        ("Quad", "d") => ("derive", "axis quads: +-coordinate of corner 0 (2 ALU); others: n . p0 >> 7 (3 mul, 6 to 13 cycles each)"),
        ("Quad", "n[3]") => ("derive", "axis quads: the axis class from the ring cross product sign; others: stored via a per-room normal table"),
        ("Quad", "material (room palette)") => ("palette", "1 mask; level index via the room palette (1 load)"),
        ("Quad", "flags") => ("range", "AXIS/axis id/neg derived from the ring; TRI from the ring length; only DOUBLE/PLANE/LOD stored"),
        ("Quad", "flags2") => ("derive", "REPEAT follows from the tile count of the predicted UVs"),
        ("RCell", "first") => ("implicit", "prefix sum of count, derived at install (resident keeps it)"),
        ("RCell", "count") => ("range", "2 ALU"),
        ("RCell", "flags") => ("range", "1 ALU"),
        ("RCell", _) => ("derive", "min/max of the cell's vertex heights, derived at install (resident keeps them for the cull)"),
        _ => ("range", ""),
    }
}

fn is_axis(q: &psx_grid::Quad) -> bool {
    q.flags & psx_grid::QUAD_AXIS != 0
}

pub fn fields(levels: &[Loaded], verbose_level: &str) {
    println!("\n# Field widths: current, measured range, minimum, chosen\n");
    let mut totals: Vec<(String, u64, u64, u64, u64, u64)> = Vec::new();
    for level in levels {
        let s = level.level().sector_units() as i32;
        let inv = ru_to_fine_table(s);
        let doc_rooms = &level.world.rooms;
        let (mut cur_all, mut min_all, mut chosen_all, mut byte_all, mut word_all) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        for r in 0..level.rooms() {
            let room = Room::from_view(&level.room(r), s);
            let cols = columns(&room, &inv);
            // Current bits: the real records.
            let cur_room: u64 = (room.sectors.len() * 160
                + room.exts.len() * 64
                + room.walls.len() * 96
                + room.splitbs.len() * 128
                + room.vertices.len() * 64
                + room.quads.len() * 320
                + room.cells.len() * 64) as u64;
            // UV prediction misses, from the probe.
            let doc = &doc_rooms[r];
            let uv_hits = room
                .quads
                .iter()
                .filter(|q| {
                    crate::probe::predict_uv_public(&room, q, doc.origin, doc.y_offset, s, &inv)
                })
                .count() as u64;
            let uv_values_missed: u64 = room
                .quads
                .iter()
                .filter(|q| {
                    !crate::probe::predict_uv_public(&room, q, doc.origin, doc.y_offset, s, &inv)
                })
                .map(|q| if q.flags & QUAD_TRI != 0 { 3 } else { 4 })
                .sum();
            let nonaxis_quads = room.quads.iter().filter(|q| !is_axis(q)).count() as u64;
            let mut min_room = 0u64;
            let mut chosen_room = 0u64;
            let mut per_table_chosen: std::collections::BTreeMap<&str, (u64, usize)> =
                Default::default();
            for col in &cols {
                let b = u64::from(col.bits());
                let values = match (col.table, col.name) {
                    ("Quad", "uv u[4]") | ("Quad", "uv v[4]") => room
                        .quads
                        .iter()
                        .map(|q| if q.flags & QUAD_TRI != 0 { 3u64 } else { 4 })
                        .sum(),
                    ("Sector", "ext index") => 0,
                    _ => col.values.len() as u64,
                };
                let range_bits = values * b;
                min_room += range_bits;
                let (how, cost) = rule(col.table, col.name);
                let chosen_bits = match how {
                    "implicit" => 0,
                    "derive" => match col.name {
                        "uv u[4]" | "uv v[4]" => uv_values_missed * b,
                        "d" => nonaxis_quads * b,
                        "n[3]" => nonaxis_quads * 3 * 8,
                        "flags2" => 0,
                        _ => 0,
                    },
                    _ => range_bits,
                };
                // Quad.flags: only the bits that are not derived.
                let chosen_bits = if col.table == "Quad" && col.name == "flags" {
                    let or = room.quads.iter().fold(0u8, |a, q| {
                        a | (q.flags
                            & !(psx_grid::QUAD_TRI
                                | psx_grid::QUAD_AXIS
                                | 0b1100
                                | psx_grid::QUAD_AXIS_NEG))
                    });
                    u64::from(or.count_ones()) * values
                } else {
                    chosen_bits
                };
                chosen_room += chosen_bits;
                let e = per_table_chosen.entry(col.table).or_default();
                e.0 += chosen_bits;
                e.1 = match col.table {
                    "Sector" => room.sectors.len(),
                    "SectorExt" => room.exts.len(),
                    "ColWall" => room.walls.len(),
                    "SplitB" => room.splitbs.len(),
                    "Vtx" => room.vertices.len(),
                    "Quad" => room.quads.len(),
                    _ => room.cells.len(),
                };
                if level.name == verbose_level {
                    let (lo, hi) = col.range();
                    println!(
                        "{:<9} {:<31} cur {:>2} b | range {:>8}..{:<8} | by range {:>2} b | chosen {:>6.2} ({:<8}) {}",
                        col.table,
                        col.name,
                        col.current,
                        lo,
                        hi,
                        b,
                        if values == 0 { 0.0 } else { chosen_bits as f64 / values as f64 },
                        how,
                        cost
                    );
                }
            }
            let _ = uv_hits;
            // Palettes the packing needs.
            let colours: HashSet<u32> = room.quads.iter().flat_map(|q| q.color).collect();
            let palette_bits = colours.len() as u64 * 24
                + room.material_use.len() as u64 * 8
                + room
                    .sectors
                    .iter()
                    .map(|x| x.flags)
                    .collect::<HashSet<_>>()
                    .len() as u64
                    * 16;
            chosen_room += palette_bits;
            min_room += palette_bits;
            byte_all += palette_bits;
            word_all += palette_bits;
            for (bits, records) in per_table_chosen.values() {
                if *records > 0 {
                    let per = (*bits as f64 / *records as f64).ceil() as u64;
                    byte_all += per.div_ceil(8) * 8 * *records as u64;
                    word_all += per.div_ceil(32) * 32 * *records as u64;
                }
            }
            cur_all += cur_room;
            min_all += min_room;
            chosen_all += chosen_room;
        }
        totals.push((
            level.name.clone(),
            cur_all,
            min_all,
            chosen_all,
            byte_all,
            word_all,
        ));
    }
    println!("\nlevel            current B | by range B (stored everything) | rule applied B (bit exact) | per-table byte records B | per-table word records B");
    for (name, cur, min, chosen, byte, word) in totals {
        println!(
            "{name:<16} {:>8} | {:>8} ({:>4.1}%) | {:>8} ({:>4.1}%) | {:>8} ({:>4.1}%) | {:>8} ({:>4.1}%)",
            cur / 8,
            min / 8,
            100.0 * min as f64 / cur as f64,
            chosen / 8,
            100.0 * chosen as f64 / cur as f64,
            byte / 8,
            100.0 * byte as f64 / cur as f64,
            word / 8,
            100.0 * word as f64 / cur as f64
        );
    }
}

/// Portals: how many are axis-aligned rectangles and what they need.
pub fn portals(levels: &[Loaded]) {
    println!("\n# Portals (36 B each today, both directions stored)\n");
    for level in levels {
        let lv = level.level();
        if lv.portals.is_empty() {
            continue;
        }
        let s = lv.sector_units() as i32;
        let click = (s / 4).max(1);
        let inv = ru_to_fine_table(s);
        let mut rect = 0usize;
        let mut sloped = 0usize;
        let mut vertical = 0usize;
        let mut max_len = 0i32;
        let (mut ylo, mut yhi) = (i32::MAX, i32::MIN);
        for p in lv.portals {
            let xs: HashSet<i16> = p.v.iter().map(|v| v[0]).collect();
            let zs: HashSet<i16> = p.v.iter().map(|v| v[2]).collect();
            let ys: HashSet<i16> = p.v.iter().map(|v| v[1]).collect();
            if p.flags & psx_grid::PORTAL_VERTICAL != 0 {
                vertical += 1;
            }
            let axis = xs.len() == 1 || zs.len() == 1;
            if axis && ys.len() <= 2 && xs.len() * zs.len() <= 2 {
                rect += 1;
            } else if axis {
                sloped += 1;
            }
            let along = xs.len().max(zs.len());
            let _ = along;
            let ext = (xs.iter().max().unwrap() - xs.iter().min().unwrap())
                .max(zs.iter().max().unwrap() - zs.iter().min().unwrap());
            max_len = max_len.max(i32::from(ext) / click);
            for y in ys {
                let f = inv.get(&i32::from(y)).copied().unwrap_or(i32::from(y));
                ylo = ylo.min(f);
                yhi = yhi.max(f);
            }
        }
        let n = lv.portals.len();
        let rooms = lv.rooms.len();
        // One record per pair: dest room, axis bit, vertical flag, fixed
        // coordinate and two extents in clicks, bottom and top at both ends.
        let room_max = lv
            .rooms
            .iter()
            .map(|r| usize::from(r.width.max(r.depth)) * 4)
            .max()
            .unwrap_or(1);
        let bits = bits_for_count(rooms as u64)
            + 2
            + bits_for_count(room_max as u64 + 1) * 3
            + bits_for_range(i64::from(ylo), i64::from(yhi)) * 4;
        println!(
            "{:<14} {} portals ({} rooms): axis-aligned rectangles {rect}, sloped {sloped}, vertical (floor/ceiling) {vertical}; \
             now {} B; one record per pair at {} bits = {} B ({:.1}x less)",
            level.name,
            n,
            rooms,
            n * 36,
            bits,
            (n / 2) * (bits as usize).div_ceil(8),
            (n * 36) as f64 / ((n / 2).max(1) * (bits as usize).div_ceil(8)) as f64
        );
    }
}

/// The resident quad if every field took the width its measured range needs
/// and the record were rounded up to whole words.
pub fn resident_quads(levels: &[Loaded]) {
    println!("\n# Resident quad record at measured widths (per level, all rooms)\n");
    println!("level           quads | bits per quad (v / colour / uv / plane / mat+flags) | words | bytes now -> packed | axis quads");
    for level in levels {
        let lv = level.level();
        let s = lv.sector_units() as i32;
        let (mut quads, mut now, mut packed, mut axis_n) = (0usize, 0usize, 0usize, 0usize);
        let mut last = String::new();
        for r in 0..level.rooms() {
            let room = Room::from_view(&level.room(r), s);
            let nq = room.quads.len();
            if nq == 0 {
                continue;
            }
            let colours: HashSet<u32> = room.quads.iter().flat_map(|q| q.color).collect();
            let u_max = room
                .quads
                .iter()
                .flat_map(|q| q.uv.map(|t| i64::from(t & 0xff)))
                .max()
                .unwrap_or(0);
            let v_max = room
                .quads
                .iter()
                .flat_map(|q| q.uv.map(|t| i64::from(t >> 8)))
                .max()
                .unwrap_or(0);
            let (dmin, dmax) = room.quads.iter().fold((i64::MAX, i64::MIN), |a, q| {
                (a.0.min(i64::from(q.d)), a.1.max(i64::from(q.d)))
            });
            let ntab: HashSet<[i8; 3]> = room
                .quads
                .iter()
                .filter(|q| q.flags & psx_grid::QUAD_AXIS == 0)
                .map(|q| q.n)
                .collect();
            let vb = bits_for_count(room.vertices.len() as u64) * 4;
            let cb = bits_for_count(colours.len() as u64) * 4;
            let ub = (bits_for_range(0, u_max) + bits_for_range(0, v_max)) * 4;
            let pb = bits_for_range(dmin, dmax) + 3 + bits_for_count(ntab.len() as u64 + 1);
            let mats: HashSet<u8> = room.quads.iter().map(|q| q.material).collect();
            let mb = bits_for_count(mats.len() as u64) + 8 + 1;
            let total = vb + cb + ub + pb + mb;
            let words = (total as usize).div_ceil(32);
            last = format!("{vb} / {cb} / {ub} / {pb} / {mb} = {total}");
            quads += nq;
            now += nq * 40;
            packed += nq * words * 4 + colours.len() * 4;
            axis_n += room
                .quads
                .iter()
                .filter(|q| q.flags & psx_grid::QUAD_AXIS != 0)
                .count();
        }
        println!(
            "{:<14} {:>5} | last room {last} | {:>2} words | {:>7} -> {:>7} ({:.0}%) | {:.0}%",
            level.name,
            quads,
            if quads == 0 {
                0
            } else {
                packed.div_ceil(quads * 4)
            },
            now,
            packed,
            100.0 * packed as f64 / now.max(1) as f64,
            100.0 * axis_n as f64 / quads.max(1) as f64
        );
    }
}
