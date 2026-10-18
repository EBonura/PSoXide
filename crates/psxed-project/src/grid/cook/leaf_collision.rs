//! The collision block of a room with leaves of several sizes
//! (`docs/grid/level-tools-design.md` 10.2): one `Sector` record per leaf,
//! the per-sector index that finds it, and the sub-block tables of sectors
//! divided into half and quarter leaves.
//!
//! Everything is read from the real leaves. A leaf larger than a sector is
//! one record whatever its size; a sector made of smaller leaves is a
//! 16 entry block; the heights, walls and split-level triangles of each leaf
//! are cooked in the leaf's own frame, so the runtime stands on exactly what
//! the preview draws (the "sector view" approximation of the earlier phases
//! is gone).

use psx_grid::write::TreeTables;
use psx_grid::{
    leaf_code, ColWall, SectorExt, SplitB, COLPLANE_PRIMARY, INDEX_SUB, MAX_LEAVES, MAX_SUBBLOCKS,
    NO_EXT, NO_LEAF, SECTOR_LEAF_SHIFT, SUBBLOCK_ENTRIES, WALL_BACK_ONLY, WALL_FRONT_ONLY,
    WALL_HALF_FIRST, WALL_HALF_SECOND,
};

use super::collision::{cliff, corners, edge_id, surface_record, Tables};
use super::plane_collision::PlaneTables;
use super::units::RuntimeUnits;
use crate::grid::leaf::{LeafKey, CLICKS_PER_SECTOR, SECTOR_LEVEL};
use crate::grid::leaf_derive::{leaf_walls, LeafWall};
use crate::grid::leaf_diag::leaf_diag_walls;
use crate::grid::overrides::WallMode;
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::tree::neighbours::along;

/// The size code a leaf of `level` is written with (see `psx_grid::leaf_code`).
pub const fn code_of(level: u8) -> u16 {
    match level {
        0 => leaf_code::X4,
        1 => leaf_code::X2,
        3 => leaf_code::HALF,
        4 => leaf_code::QUARTER,
        _ => leaf_code::SECTOR,
    }
}

/// Which part of its leaf's side a derived wall covers, as the wall flag
/// that says so (zero: the whole side).
fn span_flag(w: &LeafWall, key: LeafKey) -> u8 {
    let side = w.wall.edge;
    let (start, len) = along(key, side);
    let click = crate::grid::units::SECTOR_UNITS / CLICKS_PER_SECTOR;
    let coord = |p: [i32; 2]| -> i32 {
        let v = if matches!(side, WallEdge::North | WallEdge::South) {
            p[0]
        } else {
            p[1]
        };
        v / click
    };
    let (u0, u1) = (coord(w.geom.a), coord(w.geom.b));
    let (lo, hi) = (u0.min(u1), u0.max(u1));
    if lo == start && hi == start + len {
        return 0;
    }
    // Which half, in the wall's walking direction: north and east walk
    // towards increasing coordinates, south and west the other way.
    let forward = matches!(side, WallEdge::North | WallEdge::East);
    let low = lo == start;
    if low == forward {
        WALL_HALF_FIRST
    } else {
        WALL_HALF_SECOND
    }
}

/// Collision tables of `room`, a room whose leaves are not all whole
/// sectors. `planes` are the free-plane records already built for it.
pub fn build(
    units: &RuntimeUnits,
    room: &GridRoom,
    room_index: usize,
    planes: PlaneTables,
    mesh_warnings: Vec<String>,
) -> Result<Tables, String> {
    let s = units.sector();
    let y_offset = room.y_offset;
    let leaves: Vec<_> = room.leaves().collect();
    if leaves.len() > MAX_LEAVES {
        return Err(format!(
            "room {room_index} has {} leaves, at most {MAX_LEAVES}",
            leaves.len()
        ));
    }
    let mut tables = Tables::default();
    let mut range: Option<(i32, i32)> = planes.range;
    let mut note = |lo: i16, hi: i16| {
        range = Some(match range {
            Some((a, b)) => (a.min(i32::from(lo)), b.max(i32::from(hi))),
            None => (i32::from(lo), i32::from(hi)),
        });
    };
    let mut ids: std::collections::BTreeMap<LeafKey, u16> = std::collections::BTreeMap::new();
    for (key, doc) in &leaves {
        let id = tables.sectors.len() as u16;
        ids.insert(*key, id);
        let code = code_of(key.level);
        let edge_ru = psx_grid::leaf_edge(s as i32, code) as u32;
        let at = |what: &str| {
            format!(
                "room {room_index} cell level {} at clicks {},{}: {what}",
                key.level, key.x, key.z
            )
        };
        let (mut sector, floor_b, ceil_b) =
            surface_record(units, y_offset, doc, edge_ru, &at, &mut note)?;
        sector.flags |= code << SECTOR_LEAF_SHIFT;
        let mut walls: Vec<ColWall> = Vec::new();
        for wall_edge in WallEdge::ALL {
            for wall in doc.walls(wall_edge) {
                if key.level < SECTOR_LEVEL {
                    return Err(at("a wall on a cell larger than a sector"));
                }
                let y = corners(units, y_offset, wall.heights, &at("wall"))?;
                note(
                    y.iter().copied().min().unwrap_or(0),
                    y.iter().copied().max().unwrap_or(0),
                );
                if !wall.solid || wall.is_degenerate() {
                    continue;
                }
                walls.push(ColWall {
                    edge: edge_id(wall_edge),
                    flags: 0,
                    y,
                    reserved: 0,
                });
            }
        }
        if room.wall_mode == WallMode::Derived {
            let derived = leaf_walls(room, *key, doc)
                .into_iter()
                .chain(leaf_diag_walls(room, *key, doc));
            for w in derived {
                let Some(heights) = w.wall.edge_heights() else {
                    continue;
                };
                let y = corners(units, y_offset, heights, &at("derived wall"))?;
                let mut flags = if w.wall.blocks() {
                    0
                } else if cliff(&w.wall, &y) {
                    if w.wall.back {
                        WALL_BACK_ONLY
                    } else {
                        WALL_FRONT_ONLY
                    }
                } else {
                    continue;
                };
                if !w.wall.edge.is_diagonal() {
                    flags |= span_flag(&w, *key);
                }
                note(
                    y.iter().copied().min().unwrap_or(0),
                    y.iter().copied().max().unwrap_or(0),
                );
                walls.push(ColWall {
                    edge: edge_id(w.wall.edge),
                    flags,
                    y,
                    reserved: 0,
                });
            }
        }
        let splitb = (floor_b.is_some() || ceil_b.is_some()).then(|| SplitB {
            floor_b: floor_b.unwrap_or(sector.floor),
            ceil_b: ceil_b.unwrap_or(sector.ceil),
        });
        // Plane copies: every plane that touches any sector the leaf covers.
        let (lo, hi) = key.sector_range();
        let mut here: Vec<usize> = Vec::new();
        for z in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                if let Some([cx, cz]) = room.local([x, z]) {
                    for &i in planes
                        .per_cell
                        .get(cz * room.width + cx)
                        .into_iter()
                        .flatten()
                    {
                        if !here.contains(&i) {
                            here.push(i);
                        }
                    }
                }
            }
        }
        here.sort_unstable();
        let mut ext = SectorExt {
            splitb: NO_EXT,
            ..SectorExt::default()
        };
        if !walls.is_empty() || splitb.is_some() || !here.is_empty() {
            ext.plane_first = u16::try_from(tables.planes.len())
                .map_err(|_| format!("room {room_index} has too many plane copies"))?;
            ext.plane_count = here.len() as u8;
            for &i in &here {
                let mut copy = planes.records[i];
                if !tables.planes.iter().any(|p| p.id == copy.id) {
                    copy.flags |= COLPLANE_PRIMARY;
                }
                tables.planes.push(copy);
            }
            ext.wall_first = u16::try_from(tables.walls.len())
                .map_err(|_| format!("room {room_index} has more than 65535 solid walls"))?;
            ext.wall_count =
                u8::try_from(walls.len()).map_err(|_| at("more than 255 solid wall pieces"))?;
            tables.walls.extend(walls);
            if let Some(sb) = splitb {
                ext.splitb = u16::try_from(tables.splitbs.len())
                    .map_err(|_| format!("room {room_index} has too many split cells"))?;
                tables.splitbs.push(sb);
            }
            sector.ext = u16::try_from(tables.exts.len())
                .map_err(|_| format!("room {room_index} has too many extended cells"))?;
            tables.exts.push(ext);
        }
        tables.sectors.push(sector);
    }
    // The per-sector index and the sub-blocks.
    let mut index: Vec<u16> = Vec::with_capacity(room.width * room.depth);
    let mut blocks: Vec<u16> = Vec::new();
    for z in 0..room.depth {
        for x in 0..room.width {
            let cell = room.world_cell(x, z);
            let here = room.leaves.in_sector(cell);
            match here.as_slice() {
                [] => index.push(NO_LEAF),
                [(key, _)] if key.level <= SECTOR_LEVEL => index.push(ids[key]),
                _ => {
                    let base = [cell[0] * CLICKS_PER_SECTOR, cell[1] * CLICKS_PER_SECTOR];
                    let block = blocks.len() / SUBBLOCK_ENTRIES;
                    if block >= MAX_SUBBLOCKS {
                        return Err(format!("room {room_index} has too many divided sectors"));
                    }
                    for k in 0..SUBBLOCK_ENTRIES as i32 {
                        let click = [base[0] + (k & 3), base[1] + (k >> 2)];
                        blocks.push(match room.leaf_at(click) {
                            Some((key, _)) => ids[&key],
                            None => NO_LEAF,
                        });
                    }
                    index.push(INDEX_SUB | block as u16);
                }
            }
        }
    }
    let om = |v: i32| v.rem_euclid(4) as u8;
    tables.tree = Some(TreeTables {
        origin_mod: om(room.origin[0]) | om(room.origin[1]) << 2,
        index,
        blocks,
    });
    tables.range = range;
    tables.warnings = planes.warnings;
    tables.warnings.extend(mesh_warnings);
    Ok(tables)
}
