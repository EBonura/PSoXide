//! The collision block of a room: dense sector records plus the sparse
//! wall, extension and split-level tables.
//!
//! Heights come from the document through the same `RuntimeUnits::rt` the
//! render quads use, so what is drawn is what is stood on. The document's
//! `walkable` and `solid` flags are honoured here: a floor with `walkable`
//! off is left out of collision (a ghost floor you fall through), a ceiling
//! with it off does not block the head, a wall with `solid` off is drawn and
//! not cooked into the wall table.

use psx_grid::height::triangle_normal_q12;
use psx_grid::{
    edge, ColWall, Sector, SectorExt, SplitB, NO_EXT, SECTOR_CEIL_NESW, SECTOR_CEIL_SOLID,
    SECTOR_FLOOR_NESW, SECTOR_FLOOR_WALK_A, SECTOR_FLOOR_WALK_B, SECTOR_HAS_CEIL, SECTOR_HAS_FLOOR,
    SECTOR_OPEN_SKY, SECTOR_SPLIT_LEVEL,
};

use super::units::RuntimeUnits;
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::surface::{GridSplit, GridSurface};

/// The smallest upward normal component, Q12, a triangle may have and still
/// be stood on. The character motor uses the same number (0.7 in Q12).
pub const MIN_WALKABLE_NORMAL_Y_Q12: i16 = 2867;

/// Corner height differences above this (runtime units) make the integer
/// height function lose precision; the cook refuses them.
pub const MAX_CORNER_DELTA: i32 = 4096;

/// Collision tables of one room.
#[derive(Default)]
pub struct Tables {
    pub sectors: Vec<Sector>,
    pub exts: Vec<SectorExt>,
    pub walls: Vec<ColWall>,
    pub splitbs: Vec<SplitB>,
    /// Lowest and highest local height over the room's geometry.
    pub range: Option<(i32, i32)>,
}

fn edge_id(edge: WallEdge) -> u8 {
    match edge {
        WallEdge::North => edge::NORTH,
        WallEdge::East => edge::EAST,
        WallEdge::South => edge::SOUTH,
        WallEdge::West => edge::WEST,
        WallEdge::Nwse => edge::NWSE,
        WallEdge::Nesw => edge::NESW,
    }
}

/// Local runtime height of a grid height in a room lifted by `y_offset`.
pub fn local_height(units: &RuntimeUnits, y_offset: i32, grid: i32) -> i32 {
    units.rt(y_offset.saturating_add(grid)) - units.rt(y_offset)
}

fn to_i16(value: i32, what: &str) -> Result<i16, String> {
    i16::try_from(value).map_err(|_| format!("{what} {value} leaves the i16 range of a room"))
}

fn corners(
    units: &RuntimeUnits,
    y_offset: i32,
    heights: [i32; 4],
    what: &str,
) -> Result<[i16; 4], String> {
    let mut out = [0i16; 4];
    for (slot, h) in out.iter_mut().zip(heights) {
        *slot = to_i16(local_height(units, y_offset, h), what)?;
    }
    let (lo, hi) = (
        out.iter().copied().min().unwrap_or(0),
        out.iter().copied().max().unwrap_or(0),
    );
    if i32::from(hi) - i32::from(lo) > MAX_CORNER_DELTA {
        return Err(format!(
            "{what} corners differ by {} runtime units, at most {MAX_CORNER_DELTA}",
            i32::from(hi) - i32::from(lo)
        ));
    }
    Ok(out)
}

fn walkable_bit(heights: [i16; 4], nesw: bool, second: bool, s: u32, bit: u16) -> u16 {
    let normal = triangle_normal_q12(heights, nesw, second, s);
    if normal[1] >= MIN_WALKABLE_NORMAL_Y_Q12 {
        bit
    } else {
        0
    }
}

fn surface_flags(surface: &GridSurface) -> bool {
    matches!(surface.split, GridSplit::NeSw)
}

/// Build the collision tables of `room`. `sector` is S.
pub fn build(units: &RuntimeUnits, room: &GridRoom, room_index: usize) -> Result<Tables, String> {
    let s = units.sector();
    let mut tables = Tables::default();
    let mut range: Option<(i32, i32)> = None;
    let mut note = |lo: i16, hi: i16| {
        range = Some(match range {
            Some((a, b)) => (a.min(i32::from(lo)), b.max(i32::from(hi))),
            None => (i32::from(lo), i32::from(hi)),
        });
    };
    for z in 0..room.depth {
        for x in 0..room.width {
            let at = |what: &str| format!("room {room_index} sector {x},{z}: {what}");
            let mut sector = Sector {
                ext: NO_EXT,
                ..Sector::default()
            };
            let mut ext = SectorExt {
                splitb: NO_EXT,
                ..SectorExt::default()
            };
            let mut splitb: Option<SplitB> = None;
            let mut walls: Vec<ColWall> = Vec::new();
            if let Some(doc) = room.sector(x, z) {
                if let Some(floor) = &doc.floor {
                    let a = corners(units, room.y_offset, floor.heights, &at("floor"))?;
                    let b = floor
                        .heights_b
                        .map(|h| corners(units, room.y_offset, h, &at("floor")))
                        .transpose()?;
                    note(
                        a.iter()
                            .chain(b.iter().flatten())
                            .copied()
                            .min()
                            .unwrap_or(0),
                        a.iter()
                            .chain(b.iter().flatten())
                            .copied()
                            .max()
                            .unwrap_or(0),
                    );
                    if floor.walkable {
                        let nesw = surface_flags(floor);
                        sector.flags |= SECTOR_HAS_FLOOR;
                        if nesw {
                            sector.flags |= SECTOR_FLOOR_NESW;
                        }
                        sector.floor = a;
                        sector.flags |= walkable_bit(a, nesw, false, s, SECTOR_FLOOR_WALK_A);
                        let second = b.unwrap_or(a);
                        sector.flags |= walkable_bit(second, nesw, true, s, SECTOR_FLOOR_WALK_B);
                        if let Some(b) = b {
                            sector.flags |= SECTOR_SPLIT_LEVEL;
                            let mut sb = splitb.unwrap_or_default();
                            sb.floor_b = b;
                            splitb = Some(sb);
                        }
                    }
                }
                if let Some(ceiling) = &doc.ceiling {
                    let a = corners(units, room.y_offset, ceiling.heights, &at("ceiling"))?;
                    let b = ceiling
                        .heights_b
                        .map(|h| corners(units, room.y_offset, h, &at("ceiling")))
                        .transpose()?;
                    note(
                        a.iter()
                            .chain(b.iter().flatten())
                            .copied()
                            .min()
                            .unwrap_or(0),
                        a.iter()
                            .chain(b.iter().flatten())
                            .copied()
                            .max()
                            .unwrap_or(0),
                    );
                    sector.flags |= SECTOR_HAS_CEIL;
                    if surface_flags(ceiling) {
                        sector.flags |= SECTOR_CEIL_NESW;
                    }
                    if ceiling.walkable {
                        sector.flags |= SECTOR_CEIL_SOLID;
                    }
                    sector.ceil = a;
                    if let Some(b) = b {
                        sector.flags |= SECTOR_SPLIT_LEVEL;
                        let mut sb = splitb.unwrap_or_default();
                        sb.ceil_b = b;
                        splitb = Some(sb);
                    }
                } else if doc.floor.is_some() {
                    sector.flags |= SECTOR_OPEN_SKY;
                }
                for wall_edge in WallEdge::ALL {
                    for wall in doc.walls(wall_edge) {
                        let y = corners(units, room.y_offset, wall.heights, &at("wall"))?;
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
            }
            // A split-level sector needs both halves filled: the half the
            // document did not override repeats the surface.
            if let Some(sb) = splitb.as_mut() {
                if sector.flags & SECTOR_HAS_FLOOR != 0 && sb.floor_b == [0; 4] {
                    sb.floor_b = sector.floor;
                }
                if sector.flags & SECTOR_HAS_CEIL != 0 && sb.ceil_b == [0; 4] {
                    sb.ceil_b = sector.ceil;
                }
            }
            if !walls.is_empty() || splitb.is_some() {
                ext.wall_first = u16::try_from(tables.walls.len())
                    .map_err(|_| format!("room {room_index} has more than 65535 solid walls"))?;
                ext.wall_count =
                    u8::try_from(walls.len()).map_err(|_| at("more than 255 solid wall pieces"))?;
                tables.walls.extend(walls);
                if let Some(sb) = splitb {
                    ext.splitb = u16::try_from(tables.splitbs.len())
                        .map_err(|_| format!("room {room_index} has too many split sectors"))?;
                    tables.splitbs.push(sb);
                }
                sector.ext = u16::try_from(tables.exts.len())
                    .map_err(|_| format!("room {room_index} has too many extended sectors"))?;
                tables.exts.push(ext);
            }
            tables.sectors.push(sector);
        }
    }
    tables.range = range;
    Ok(tables)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::{GridSurface, GridWall};
    use crate::grid::units::GridScale;

    fn units() -> RuntimeUnits {
        RuntimeUnits::new(GridScale { num: 5, den: 4 }).unwrap()
    }

    #[test]
    fn heights_flags_and_ext_follow_the_document() {
        let mut room = GridRoom::new("r", 2, 1);
        {
            let sector = room.sector_or_insert(0, 0).unwrap();
            sector.floor = Some(GridSurface::flat(256, None));
            let mut wall = GridWall::span(0, 1024, None);
            wall.solid = true;
            sector.walls_mut(WallEdge::East).push(wall);
        }
        {
            let sector = room.sector_or_insert(1, 0).unwrap();
            let mut floor = GridSurface::flat(0, None);
            floor.walkable = false;
            sector.floor = Some(floor);
            sector.ceiling = Some(GridSurface::flat(1024, None));
        }
        let tables = build(&units(), &room, 0).unwrap();
        assert_eq!(tables.sectors.len(), 2);
        let a = tables.sectors[0];
        assert_ne!(a.flags & SECTOR_HAS_FLOOR, 0);
        assert_eq!(a.floor, [20; 4]);
        assert_ne!(a.flags & SECTOR_FLOOR_WALK_A, 0);
        assert_ne!(a.flags & SECTOR_OPEN_SKY, 0);
        assert_eq!(tables.exts.len(), 1);
        assert_eq!(tables.walls.len(), 1);
        assert_eq!(tables.walls[0].edge, edge::EAST);
        assert_eq!(tables.walls[0].y, [0, 0, 80, 80]);
        let b = tables.sectors[1];
        // Ghost floor, solid ceiling, no sky.
        assert_eq!(b.flags & SECTOR_HAS_FLOOR, 0);
        assert_ne!(b.flags & SECTOR_CEIL_SOLID, 0);
        assert_eq!(b.flags & SECTOR_OPEN_SKY, 0);
        assert_eq!(b.ext, NO_EXT);
    }

    #[test]
    fn steep_triangles_are_not_walkable_and_non_solid_walls_are_left_out() {
        let mut room = GridRoom::new("r", 1, 1);
        let sector = room.sector_or_insert(0, 0).unwrap();
        let mut floor = GridSurface::flat(0, None);
        floor.heights = [0, 0, 2048, 2048];
        sector.floor = Some(floor);
        let mut passable = GridWall::span(0, 256, None);
        passable.solid = false;
        sector.walls_mut(WallEdge::North).push(passable);
        let tables = build(&units(), &room, 0).unwrap();
        // Two sectors of rise over one is steeper than the 0.7 normal limit.
        assert_eq!(tables.sectors[0].flags & SECTOR_FLOOR_WALK_A, 0);
        assert!(tables.walls.is_empty());
        assert_eq!(tables.sectors[0].ext, NO_EXT);
    }

    #[test]
    fn split_level_floors_get_a_second_height_record() {
        let mut room = GridRoom::new("r", 1, 1);
        let sector = room.sector_or_insert(0, 0).unwrap();
        let mut floor = GridSurface::flat(0, None);
        floor.heights_b = Some([256; 4]);
        sector.floor = Some(floor);
        let tables = build(&units(), &room, 0).unwrap();
        assert_ne!(tables.sectors[0].flags & SECTOR_SPLIT_LEVEL, 0);
        assert_eq!(tables.splitbs[0].floor_b, [20; 4]);
    }
}
