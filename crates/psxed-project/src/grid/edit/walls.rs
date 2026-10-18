//! Placing walls along sector edges.
//!
//! A new wall fills one vertical gap on its edge: between the sector's own
//! floor and ceiling along that edge, or between neighbouring walls already
//! stacked there. Heights are taken corner by corner, so a wall over a
//! sloped floor or under a sloped ceiling follows the slope, and where the
//! gap closes at one end the wall becomes a triangle. Where the sector has
//! no floor or ceiling the room's own height bounds stand in. This is the
//! behaviour of the bonnie-32 wall tool, in integers.

use crate::ResourceId;

use super::super::derive;
use super::super::limits::MAX_WALLS_PER_EDGE;
use super::super::overrides::WallMode;
use super::super::room::GridRoom;
use super::super::sector::{GridSector, WallEdge};
use super::super::surface::{GridSurface, GridWall, WallFacing};
use super::super::units::{CLICK_UNITS, SECTOR_UNITS};
use super::super::world::GridWorld;
use super::rects::{grow_room_to, CellRect, RectRefusal};
use super::refs::FaceRef;

/// Smallest vertical gap a wall is offered for: one height click.
const MIN_GAP: i32 = CLICK_UNITS;
/// Height of a room's ceiling when it has no geometry to measure.
const DEFAULT_ROOM_HEIGHT: i32 = 3 * SECTOR_UNITS;

/// Corner numbers `[a, b]` of a sector's surfaces that an edge runs between.
pub const fn edge_corner_indices(edge: WallEdge) -> [usize; 2] {
    match edge {
        WallEdge::North => [0, 1],
        WallEdge::East => [1, 2],
        WallEdge::South => [2, 3],
        WallEdge::West => [3, 0],
        WallEdge::Nwse => [0, 2],
        WallEdge::Nesw => [1, 3],
    }
}

/// The triangle of `surface` that holds both ends of `edge` (the first one
/// when the edge is a fold, which both hold).
pub fn edge_triangle(surface: &GridSurface, edge: WallEdge) -> usize {
    let [a, b] = edge_corner_indices(edge);
    (0..2)
        .find(|t| {
            let corners = surface.split.triangle(*t);
            corners.contains(&a) && corners.contains(&b)
        })
        .unwrap_or(0)
}

/// Heights of a surface at the two ends `a`, `b` of `edge`. A split-level
/// surface answers with the triangle that actually contains the edge.
pub fn surface_edge_heights(surface: &GridSurface, edge: WallEdge) -> [i32; 2] {
    let [a, b] = edge_corner_indices(edge);
    let heights = surface.heights_for(edge_triangle(surface, edge));
    [heights[a], heights[b]]
}

/// Lowest and highest geometry height of a room, room-relative. A room with
/// less than one click of vertical extent is given the default ceiling
/// height above its bottom.
pub fn room_height_bounds(room: &GridRoom) -> (i32, i32) {
    let mut range: Option<(i32, i32)> = None;
    for (_, sector) in room.leaves() {
        if let Some((lo, hi)) = sector.height_range() {
            range = Some(range.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        }
    }
    let (bottom, top) = range.unwrap_or((0, 0));
    if top - bottom < MIN_GAP {
        (bottom, bottom + DEFAULT_ROOM_HEIGHT)
    } else {
        (bottom, top)
    }
}

/// One fillable gap: corner heights and the span used to choose between gaps.
struct Gap {
    heights: [i32; 4],
    bottom: i32,
    top: i32,
}

/// Heights `[bottom a, bottom b, top b, top a]` of the wall that would be
/// placed on `edge` of `sector`, or `None` when the edge is fully covered.
///
/// `pick_y` is a room-relative height that chooses between several gaps (the
/// nearest wins) and between the lower and upper triangle on a sloped edge;
/// `None` takes the tallest gap.
pub fn next_wall_heights(
    sector: &GridSector,
    edge: WallEdge,
    room_bounds: (i32, i32),
    pick_y: Option<i32>,
) -> Option<[i32; 4]> {
    let (room_bottom, room_top) = room_bounds;
    let [fa, fb] = sector
        .ground_floor()
        .map_or([room_bottom; 2], |s| surface_edge_heights(s, edge));
    let [ca, cb] = sector
        .ground_ceiling()
        .map_or([room_top; 2], |s| surface_edge_heights(s, edge));
    let walls = sector.walls(edge);
    if walls.len() >= MAX_WALLS_PER_EDGE {
        return None;
    }

    if walls.is_empty() {
        if (fa - fb).abs() > MIN_GAP || (ca - cb).abs() > MIN_GAP {
            // A sloped edge: offer the triangle under or over the slope.
            let floor_high = fa.max(fb);
            let ceiling_low = ca.min(cb);
            if let Some(y) = pick_y {
                return Some(if y < (floor_high + ceiling_low) / 2 {
                    [fa, fb, floor_high, floor_high]
                } else {
                    [floor_high, floor_high, cb, ca]
                });
            }
        }
        return Some([fa, fb, cb, ca]);
    }

    let mut sorted: Vec<&GridWall> = walls.iter().collect();
    sorted.sort_by_key(|w| w.heights[0] + w.heights[1]);
    let mut gaps: Vec<Gap> = Vec::new();

    // Floor up to the lowest wall, closing a triangle where one end touches.
    let lowest = sorted[0];
    let (gap_a, gap_b) = (lowest.heights[0] - fa, lowest.heights[1] - fb);
    if gap_a.max(gap_b) > MIN_GAP {
        let top_a = if gap_a > MIN_GAP {
            lowest.heights[0]
        } else {
            fa
        };
        let top_b = if gap_b > MIN_GAP {
            lowest.heights[1]
        } else {
            fb
        };
        gaps.push(Gap {
            heights: [fa, fb, top_b, top_a],
            bottom: (fa + fb) / 2,
            top: (top_a + top_b) / 2,
        });
    }
    // Between neighbouring walls.
    for pair in sorted.windows(2) {
        let (lower, upper) = (pair[0], pair[1]);
        let (gap_a, gap_b) = (
            upper.heights[0] - lower.heights[3],
            upper.heights[1] - lower.heights[2],
        );
        if gap_a.max(gap_b) > MIN_GAP {
            gaps.push(Gap {
                heights: [
                    lower.heights[3],
                    lower.heights[2],
                    upper.heights[1],
                    upper.heights[0],
                ],
                bottom: (lower.heights[2] + lower.heights[3]) / 2,
                top: (upper.heights[0] + upper.heights[1]) / 2,
            });
        }
    }
    // The highest wall up to the ceiling.
    let highest = sorted[sorted.len() - 1];
    let (gap_a, gap_b) = (ca - highest.heights[3], cb - highest.heights[2]);
    if gap_a.max(gap_b) > MIN_GAP {
        let bottom_a = if gap_a > MIN_GAP {
            highest.heights[3]
        } else {
            ca
        };
        let bottom_b = if gap_b > MIN_GAP {
            highest.heights[2]
        } else {
            cb
        };
        gaps.push(Gap {
            heights: [bottom_a, bottom_b, cb, ca],
            bottom: (bottom_a + bottom_b) / 2,
            top: (ca + cb) / 2,
        });
    }

    match pick_y {
        Some(y) => gaps
            .into_iter()
            .min_by_key(|g| (y - (g.bottom + g.top) / 2).abs()),
        None => gaps.into_iter().max_by_key(|g| g.top - g.bottom),
    }
    .map(|g| g.heights)
}

/// The wall that would be placed on `edge` of world sector `cell` of
/// `room`: [`next_wall_heights`] on the sector as it is, with the faces a
/// derived room already has on that edge counted as walls already standing
/// there, so an edge a derived wall closes answers "covered" instead of
/// getting a second wall drawn on top of it.
pub fn wall_gap(
    room: &GridRoom,
    cell: [i32; 2],
    edge: WallEdge,
    pick_y: Option<i32>,
) -> Option<[i32; 4]> {
    let mut sector = room
        .local(cell)
        .and_then(|[x, z]| room.sector(x, z))
        .cloned()
        .unwrap_or_default();
    if room.wall_mode == WallMode::Derived {
        sector
            .walls_mut(edge)
            .extend(derive::virtual_walls(room, cell, edge));
    }
    next_wall_heights(&sector, edge, room_height_bounds(room), pick_y)
}

/// How a placed wall looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallSpec {
    /// Visible side.
    pub facing: WallFacing,
    /// Material.
    pub material: Option<ResourceId>,
}

/// Why a wall was not placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallRefusal {
    /// The room index does not exist.
    NoSuchRoom,
    /// The edge has no gap left to fill.
    Covered,
    /// Placing it would grow the room past the size cap.
    RoomTooLarge,
    /// The cell is larger than a sector, which carries no authored walls
    /// (subdivide it first), or there is no cell there.
    BigCell,
}

/// Place one wall on `edge` of `cell`, creating the sector (and growing the
/// room) when it does not exist yet.
pub fn place_wall(
    world: &mut GridWorld,
    room: usize,
    cell: [i32; 2],
    edge: WallEdge,
    pick_y: Option<i32>,
    spec: WallSpec,
) -> Result<FaceRef, WallRefusal> {
    let target = world.rooms.get_mut(room).ok_or(WallRefusal::NoSuchRoom)?;
    if !target.leaves.is_sectors_only() {
        return place_wall_in_sector(world, room, cell, edge, pick_y, spec);
    }
    // Decide first on a sector that may not exist yet, so a covered edge
    // leaves the room exactly as it was.
    let heights = wall_gap(target, cell, edge, pick_y).ok_or(WallRefusal::Covered)?;
    match grow_room_to(target, CellRect::from_corners(cell, cell)) {
        Ok(_) => {}
        Err(RectRefusal::NoSuchRoom) => return Err(WallRefusal::NoSuchRoom),
        Err(RectRefusal::RoomTooLarge { .. }) => return Err(WallRefusal::RoomTooLarge),
    }
    let [x, z] = target.local(cell).ok_or(WallRefusal::RoomTooLarge)?;
    let sector = target
        .sector_or_insert(x, z)
        .ok_or(WallRefusal::RoomTooLarge)?;
    let stack = sector.walls_mut(edge);
    stack.push(GridWall {
        facing: spec.facing,
        ..GridWall::span(0, 0, spec.material)
    });
    let index = stack.len() - 1;
    stack[index].heights = heights;
    Ok(FaceRef::wall(room, cell, edge, index))
}

/// The wall that would be placed on `edge` of the leaf `key` (any size up
/// to a sector): [`next_wall_heights`] on the leaf as it is, with the derived
/// faces already standing on that side counted as walls, as [`wall_gap`] does
/// for a sector.
pub fn leaf_wall_gap(
    room: &GridRoom,
    key: super::super::leaf::LeafKey,
    edge: WallEdge,
    pick_y: Option<i32>,
) -> Option<[i32; 4]> {
    let mut sector = room.leaf(key)?.clone();
    if room.wall_mode == WallMode::Derived {
        for wall in super::super::leaf_derive::derived_walls_of(room, key) {
            if wall.edge == edge {
                if let Some(w) = wall.as_wall_of(key.cell(), edge) {
                    sector.walls_mut(edge).push(w);
                }
            }
        }
    }
    next_wall_heights(&sector, edge, room_height_bounds(room), pick_y)
}

/// Place one wall on `edge` of the leaf `key` (a cell of a sector or
/// smaller), filling the gap [`leaf_wall_gap`] finds.
pub fn place_wall_leaf(
    world: &mut GridWorld,
    room: usize,
    key: super::super::leaf::LeafKey,
    edge: WallEdge,
    pick_y: Option<i32>,
    spec: WallSpec,
) -> Result<FaceRef, WallRefusal> {
    let target = world.rooms.get_mut(room).ok_or(WallRefusal::NoSuchRoom)?;
    if key.level < super::super::leaf::SECTOR_LEVEL || target.leaf(key).is_none() {
        return Err(WallRefusal::BigCell);
    }
    let heights = leaf_wall_gap(target, key, edge, pick_y).ok_or(WallRefusal::Covered)?;
    let sector = target.leaf_mut(key).ok_or(WallRefusal::BigCell)?;
    let stack = sector.walls_mut(edge);
    stack.push(GridWall {
        facing: spec.facing,
        ..GridWall::span(0, 0, spec.material)
    });
    let index = stack.len() - 1;
    stack[index].heights = heights;
    Ok(FaceRef::new(
        room,
        key,
        super::refs::FaceTarget::Wall { edge, index },
    ))
}

/// [`place_wall`] in a room with cells of several sizes: the wall goes on the
/// side of `edge` of world sector `cell` on every cell that touches that
/// side (a whole sector, or the smaller cells along it). A cell larger than a
/// sector is split to sectors first (lossless). Returns the first wall.
fn place_wall_in_sector(
    world: &mut GridWorld,
    room: usize,
    cell: [i32; 2],
    edge: WallEdge,
    pick_y: Option<i32>,
    spec: WallSpec,
) -> Result<FaceRef, WallRefusal> {
    use super::super::leaf::{LeafKey, CLICKS_PER_SECTOR, SECTOR_LEVEL};
    let target = world.rooms.get_mut(room).ok_or(WallRefusal::NoSuchRoom)?;
    let base = [cell[0] * CLICKS_PER_SECTOR, cell[1] * CLICKS_PER_SECTOR];
    if let Some((key, _)) = target.leaf_at([base[0] + 1, base[1] + 1]) {
        if key.level < SECTOR_LEVEL {
            super::super::tree::subdivide_all_to(target, &[key], SECTOR_LEVEL);
        }
    }
    // The clicks of the sector along the side.
    let n = CLICKS_PER_SECTOR;
    let clicks: Vec<[i32; 2]> = (0..n)
        .map(|i| match edge {
            WallEdge::North => [base[0] + i, base[1]],
            WallEdge::South => [base[0] + i, base[1] + n - 1],
            WallEdge::West => [base[0], base[1] + i],
            WallEdge::East => [base[0] + n - 1, base[1] + i],
            _ => [base[0] + 1, base[1] + 1],
        })
        .collect();
    let mut keys: Vec<LeafKey> = Vec::new();
    for click in clicks {
        if let Some((key, _)) = world.rooms[room].leaf_at(click) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    let mut first = None;
    let mut covered = false;
    for key in keys {
        match place_wall_leaf(world, room, key, edge, pick_y, spec) {
            Ok(face) => {
                first.get_or_insert(face);
            }
            Err(WallRefusal::Covered) => covered = true,
            Err(other) => return Err(other),
        }
    }
    first.ok_or(if covered {
        WallRefusal::Covered
    } else {
        WallRefusal::BigCell
    })
}

/// What a run of walls did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOutcome {
    /// Walls created.
    pub placed: Vec<FaceRef>,
    /// Edges that had nothing left to fill.
    pub covered: usize,
    /// Set when the room would have outgrown its cap.
    pub too_large: bool,
}

/// Place a wall on `edge` of every cell in `cells`.
pub fn place_wall_run(
    world: &mut GridWorld,
    room: usize,
    cells: &[[i32; 2]],
    edge: WallEdge,
    pick_y: Option<i32>,
    spec: WallSpec,
) -> RunOutcome {
    let mut out = RunOutcome::default();
    for cell in cells {
        match place_wall(world, room, *cell, edge, pick_y, spec) {
            Ok(face) => out.placed.push(face),
            Err(WallRefusal::Covered) => out.covered += 1,
            Err(WallRefusal::RoomTooLarge) => out.too_large = true,
            Err(WallRefusal::NoSuchRoom) => break,
            Err(WallRefusal::BigCell) => out.covered += 1,
        }
    }
    out
}

/// The cells a wall drag from `start` to `end` covers. Cardinal walls lock
/// to the edge's own axis (north and south walls run along X); diagonal
/// walls lock to 45 degrees along the longer pointer travel.
pub fn wall_run_cells(start: [i32; 2], end: [i32; 2], edge: WallEdge) -> Vec<[i32; 2]> {
    let (dx, dz) = (end[0] - start[0], end[1] - start[1]);
    match edge {
        WallEdge::North | WallEdge::South => {
            let (lo, hi) = (start[0].min(end[0]), start[0].max(end[0]));
            (lo..=hi).map(|x| [x, start[1]]).collect()
        }
        WallEdge::East | WallEdge::West => {
            let (lo, hi) = (start[1].min(end[1]), start[1].max(end[1]));
            (lo..=hi).map(|z| [start[0], z]).collect()
        }
        WallEdge::Nwse | WallEdge::Nesw => {
            let length = dx.abs().max(dz.abs());
            let primary = if dx.abs() >= dz.abs() { dx } else { dz };
            let sign = if primary >= 0 { 1 } else { -1 };
            // NW-SE: x and z move together. NE-SW: they move opposite.
            let step = if edge == WallEdge::Nwse {
                [sign, sign]
            } else if dx.abs() >= dz.abs() {
                [sign, -sign]
            } else {
                [-sign, sign]
            };
            (0..=length)
                .map(|i| [start[0] + step[0] * i, start[1] + step[1] * i])
                .collect()
        }
    }
}

/// Which side of a new wall faces `viewer` (a position in world grid units
/// `[x, z]`): `Front` when the viewer is on the owning sector's inner side.
pub fn facing_toward(cell: [i32; 2], edge: WallEdge, viewer: [i64; 2]) -> WallFacing {
    let (a, b) = edge.endpoints();
    let mid = [
        (i64::from(cell[0]) * 2 + i64::from(a[0] + b[0])) * i64::from(SECTOR_UNITS) / 2,
        (i64::from(cell[1]) * 2 + i64::from(a[1] + b[1])) * i64::from(SECTOR_UNITS) / 2,
    ];
    let front = edge.front_direction();
    let dot =
        i64::from(front[0]) * (viewer[0] - mid[0]) + i64::from(front[1]) * (viewer[1] - mid[1]);
    if dot >= 0 {
        WallFacing::Front
    } else {
        WallFacing::Back
    }
}

/// Remove the wall on `edge` of `cell` whose middle is nearest `pick_y`, or
/// the topmost one when `pick_y` is `None`. Returns whether one was removed.
pub fn remove_wall(
    world: &mut GridWorld,
    room: usize,
    cell: [i32; 2],
    edge: WallEdge,
    pick_y: Option<i32>,
) -> bool {
    let Some(target) = world.rooms.get_mut(room) else {
        return false;
    };
    let Some([x, z]) = target.local(cell) else {
        return false;
    };
    let Some(sector) = target.sector_mut(x, z) else {
        return false;
    };
    if !remove_nearest_wall(sector.walls_mut(edge), pick_y) {
        return false;
    }
    target.trim_empty();
    true
}

/// [`remove_wall`] on the leaf `key`, a cell of a sector or smaller.
pub fn remove_wall_leaf(
    world: &mut GridWorld,
    room: usize,
    key: super::super::leaf::LeafKey,
    edge: WallEdge,
    pick_y: Option<i32>,
) -> bool {
    let Some(target) = world.rooms.get_mut(room) else {
        return false;
    };
    let Some(sector) = target.leaf_mut(key) else {
        return false;
    };
    if !remove_nearest_wall(sector.walls_mut(edge), pick_y) {
        return false;
    }
    target.trim_empty();
    true
}

/// Remove the wall of `stack` nearest `pick_y` (the highest one when `None`).
fn remove_nearest_wall(stack: &mut Vec<GridWall>, pick_y: Option<i32>) -> bool {
    let middle = |w: &GridWall| (w.heights.iter().sum::<i32>()) / 4;
    let index = match pick_y {
        Some(y) => stack
            .iter()
            .enumerate()
            .min_by_key(|(_, w)| (middle(w) - y).abs())
            .map(|(i, _)| i),
        None => stack
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| middle(w))
            .map(|(i, _)| i),
    };
    match index {
        Some(index) => {
            stack.remove(index);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::GridSplit;
    use crate::grid::units::GridScale;

    fn sector(floor: Option<[i32; 4]>, ceiling: Option<[i32; 4]>) -> GridSector {
        let surface = |heights| {
            let mut s = GridSurface::flat(0, None);
            s.heights = heights;
            s
        };
        GridSector::with_surfaces(floor.map(surface), ceiling.map(surface))
    }

    const BOUNDS: (i32, i32) = (0, 3072);

    #[test]
    fn an_empty_edge_fills_floor_to_ceiling() {
        let s = sector(Some([0; 4]), Some([2048; 4]));
        assert_eq!(
            next_wall_heights(&s, WallEdge::North, BOUNDS, None),
            Some([0, 0, 2048, 2048])
        );
    }

    #[test]
    fn missing_surfaces_fall_back_to_the_room_bounds() {
        let s = sector(None, None);
        assert_eq!(
            next_wall_heights(&s, WallEdge::East, (256, 2560), None),
            Some([256, 256, 2560, 2560])
        );
    }

    #[test]
    fn walls_follow_a_sloped_floor_and_ceiling_at_their_own_edge() {
        // Floor corners NW,NE,SE,SW = 0, 0, 512, 512: rises to the south.
        let s = sector(Some([0, 0, 512, 512]), Some([2048, 2048, 2048, 2048]));
        assert_eq!(
            next_wall_heights(&s, WallEdge::North, BOUNDS, None),
            Some([0, 0, 2048, 2048])
        );
        // South edge a = SE, b = SW.
        assert_eq!(
            next_wall_heights(&s, WallEdge::South, BOUNDS, None),
            Some([512, 512, 2048, 2048])
        );
        // East edge a = NE (0), b = SE (512): the bottom rail slopes.
        assert_eq!(
            next_wall_heights(&s, WallEdge::East, BOUNDS, None),
            Some([0, 512, 2048, 2048])
        );
    }

    #[test]
    fn a_steep_edge_offers_the_triangle_under_or_over_the_slope() {
        let s = sector(Some([0, 1024, 1024, 0]), Some([3072; 4]));
        // North edge slopes from 0 up to 1024: more than a click apart.
        assert_eq!(
            next_wall_heights(&s, WallEdge::North, BOUNDS, Some(100)),
            Some([0, 1024, 1024, 1024]),
            "below the middle: the triangle under the slope's top"
        );
        assert_eq!(
            next_wall_heights(&s, WallEdge::North, BOUNDS, Some(2800)),
            Some([1024, 1024, 3072, 3072]),
            "above the middle: the wall over the high end"
        );
        // Without a preference the whole sloped span is filled.
        assert_eq!(
            next_wall_heights(&s, WallEdge::North, BOUNDS, None),
            Some([0, 1024, 3072, 3072])
        );
    }

    #[test]
    fn stacked_walls_fill_the_remaining_gaps_one_at_a_time() {
        let mut s = sector(Some([0; 4]), Some([3072; 4]));
        s.north.push(GridWall::span(1024, 2048, None));
        // Two gaps: 0..1024 and 2048..3072. Both are 1024 tall; a pick
        // chooses, no pick takes the first largest.
        assert_eq!(
            next_wall_heights(&s, WallEdge::North, BOUNDS, Some(100)),
            Some([0, 0, 1024, 1024])
        );
        assert_eq!(
            next_wall_heights(&s, WallEdge::North, BOUNDS, Some(2900)),
            Some([2048, 2048, 3072, 3072])
        );
        s.north.push(GridWall::span(0, 1024, None));
        assert_eq!(
            next_wall_heights(&s, WallEdge::North, BOUNDS, Some(100)),
            Some([2048, 2048, 3072, 3072]),
            "the lower gap is taken, so the only one left is chosen"
        );
        s.north.push(GridWall::span(2048, 3072, None));
        assert_eq!(next_wall_heights(&s, WallEdge::North, BOUNDS, None), None);
    }

    #[test]
    fn gaps_smaller_than_a_click_are_not_offered() {
        let mut s = sector(Some([0; 4]), Some([1024; 4]));
        s.west.push(GridWall::span(0, 1024 - CLICK_UNITS, None));
        // Exactly one click left: not more than MIN_GAP, so covered.
        assert_eq!(next_wall_heights(&s, WallEdge::West, BOUNDS, None), None);
        s.west[0] = GridWall::span(0, 1024 - CLICK_UNITS - 64, None);
        assert_eq!(
            next_wall_heights(&s, WallEdge::West, BOUNDS, None),
            Some([704, 704, 1024, 1024])
        );
    }

    #[test]
    fn a_closed_end_becomes_a_triangle() {
        let mut s = sector(Some([0; 4]), Some([3072; 4]));
        // A slanted wall that touches the ceiling at b but leaves a gap at a.
        let mut slanted = GridWall::span(0, 0, None);
        slanted.heights = [0, 0, 3072, 1024];
        s.north.push(slanted);
        // Gap above: at a (corner 3) 1024..3072, at b none.
        assert_eq!(
            next_wall_heights(&s, WallEdge::North, BOUNDS, None),
            Some([1024, 3072, 3072, 3072])
        );
    }

    #[test]
    fn diagonal_walls_use_the_diagonal_corners() {
        let s = sector(Some([0, 512, 1024, 0]), Some([3072; 4]));
        // NW-SE joins corners 0 and 2.
        assert_eq!(
            next_wall_heights(&s, WallEdge::Nwse, BOUNDS, None),
            Some([0, 1024, 3072, 3072])
        );
        // NE-SW joins corners 1 and 3.
        assert_eq!(
            next_wall_heights(&s, WallEdge::Nesw, BOUNDS, None),
            Some([512, 0, 3072, 3072])
        );
    }

    #[test]
    fn a_split_level_surface_answers_with_the_triangle_holding_the_edge() {
        let mut floor = GridSurface::flat(0, None);
        floor.split = GridSplit::NwSe;
        floor.heights_b = Some([512; 4]);
        // North is in triangle 0 (NW,NE,SE), south in triangle 1.
        assert_eq!(surface_edge_heights(&floor, WallEdge::North), [0, 0]);
        assert_eq!(surface_edge_heights(&floor, WallEdge::South), [512, 512]);
        floor.split = GridSplit::NeSw;
        // NE-SW: triangle 0 is NW,NE,SW (north and west), 1 is NE,SE,SW.
        assert_eq!(surface_edge_heights(&floor, WallEdge::West), [0, 0]);
        assert_eq!(surface_edge_heights(&floor, WallEdge::East), [512, 512]);
    }

    #[test]
    fn room_bounds_come_from_the_geometry() {
        let mut room = GridRoom::new("r", 1, 1);
        assert_eq!(room_height_bounds(&room), (0, 3072));
        let s = room.sector_or_insert(0, 0).unwrap();
        s.set_ground_floor(Some(GridSurface::flat(-256, None)));
        s.set_ground_ceiling(Some(GridSurface::flat(1024, None)));
        assert_eq!(room_height_bounds(&room), (-256, 1024));
        // A flat room still gets a default ceiling over its floor.
        let mut flat = GridRoom::new("f", 1, 1);
        flat.sector_or_insert(0, 0)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(512, None)));
        assert_eq!(room_height_bounds(&flat), (512, 512 + 3072));
    }

    fn world() -> GridWorld {
        let mut room = GridRoom::new("r", 1, 1);
        let s = room.sector_or_insert(0, 0).unwrap();
        s.set_ground_floor(Some(GridSurface::flat(0, None)));
        s.set_ground_ceiling(Some(GridSurface::flat(2048, None)));
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    const SPEC: WallSpec = WallSpec {
        facing: WallFacing::Front,
        material: None,
    };

    #[test]
    fn placing_a_wall_on_the_room_edge_and_beyond() {
        let mut world = world();
        let face = place_wall(&mut world, 0, [0, 0], WallEdge::North, None, SPEC).unwrap();
        assert_eq!(face.heights(&world, 0), Some([0, 0, 2048, 2048]));
        assert_eq!(
            place_wall(&mut world, 0, [0, 0], WallEdge::North, None, SPEC),
            Err(WallRefusal::Covered)
        );
        // A cell outside the room grows it and gets a hollow sector.
        let far = place_wall(&mut world, 0, [-2, 1], WallEdge::East, None, SPEC).unwrap();
        let room = &world.rooms[0];
        assert_eq!(room.origin, [-2, 0]);
        assert_eq!((room.width, room.depth), (3, 2));
        assert!(far.exists(&world));
        // Room bounds stand in where the new sector has no surfaces.
        assert_eq!(far.heights(&world, 0), Some([0, 0, 2048, 2048]));
    }

    #[test]
    fn a_covered_edge_leaves_the_room_untouched() {
        let mut world = world();
        for _ in 0..1 {
            place_wall(&mut world, 0, [0, 0], WallEdge::South, None, SPEC).unwrap();
        }
        let before = world.clone();
        let out = place_wall_run(&mut world, 0, &[[0, 0]], WallEdge::South, None, SPEC);
        assert_eq!((out.placed.len(), out.covered), (0, 1));
        assert_eq!(world, before);
    }

    #[test]
    fn runs_lock_to_the_edge_axis() {
        assert_eq!(
            wall_run_cells([2, 5], [-1, 9], WallEdge::North),
            vec![[-1, 5], [0, 5], [1, 5], [2, 5]]
        );
        assert_eq!(
            wall_run_cells([2, 5], [7, 3], WallEdge::East),
            vec![[2, 3], [2, 4], [2, 5]]
        );
    }

    #[test]
    fn diagonal_runs_lock_to_forty_five_degrees() {
        assert_eq!(
            wall_run_cells([0, 0], [3, 1], WallEdge::Nwse),
            vec![[0, 0], [1, 1], [2, 2], [3, 3]]
        );
        assert_eq!(
            wall_run_cells([0, 0], [-1, -3], WallEdge::Nwse),
            vec![[0, 0], [-1, -1], [-2, -2], [-3, -3]]
        );
        assert_eq!(
            wall_run_cells([0, 0], [2, 0], WallEdge::Nesw),
            vec![[0, 0], [1, -1], [2, -2]]
        );
        assert_eq!(
            wall_run_cells([0, 0], [0, 2], WallEdge::Nesw),
            vec![[0, 0], [-1, 1], [-2, 2]]
        );
    }

    #[test]
    fn a_run_places_one_wall_per_cell_and_counts_covered_edges() {
        let mut room = GridRoom::new("r", 4, 1);
        for x in 0..4 {
            let s = room.sector_or_insert(x, 0).unwrap();
            s.set_ground_floor(Some(GridSurface::flat(0, None)));
            s.set_ground_ceiling(Some(GridSurface::flat(1024, None)));
        }
        let mut world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        };
        place_wall(&mut world, 0, [2, 0], WallEdge::North, None, SPEC).unwrap();
        let cells = wall_run_cells([0, 0], [3, 0], WallEdge::North);
        let out = place_wall_run(&mut world, 0, &cells, WallEdge::North, None, SPEC);
        assert_eq!((out.placed.len(), out.covered), (3, 1));
    }

    #[test]
    fn facing_follows_the_side_the_viewer_stands_on() {
        // North wall of cell (0,0) sits at z = 0 and faces +z (into the cell).
        assert_eq!(
            facing_toward([0, 0], WallEdge::North, [512, 800]),
            WallFacing::Front
        );
        assert_eq!(
            facing_toward([0, 0], WallEdge::North, [512, -800]),
            WallFacing::Back
        );
        assert_eq!(
            facing_toward([1, 0], WallEdge::West, [100, 512]),
            WallFacing::Back
        );
    }

    #[test]
    fn removal_picks_the_wall_nearest_the_pointer_height() {
        let mut world = world();
        let s = world.rooms[0].sector_mut(0, 0).unwrap();
        s.south.push(GridWall::span(0, 512, None));
        s.south.push(GridWall::span(1024, 2048, None));
        assert!(remove_wall(
            &mut world,
            0,
            [0, 0],
            WallEdge::South,
            Some(1500)
        ));
        let s = world.rooms[0].sector(0, 0).unwrap();
        assert_eq!(s.south.len(), 1);
        assert_eq!(s.south[0].heights[3], 512);
        assert!(remove_wall(&mut world, 0, [0, 0], WallEdge::South, None));
        assert!(!remove_wall(&mut world, 0, [0, 0], WallEdge::South, None));
    }

    #[test]
    fn derived_faces_count_as_walls_already_standing() {
        // A derived 3 x 3 hall, floors 0, ceilings 2048, a platform of 512
        // in the middle.
        let mut room = GridRoom::new_derived("hall", 3, 3);
        for z in 0..3 {
            for x in 0..3 {
                let sector = room.sector_or_insert(x, z).unwrap();
                sector.set_ground_floor(Some(GridSurface::flat(0, None)));
                sector.set_ground_ceiling(Some(GridSurface::flat(2048, None)));
            }
        }
        room.sector_or_insert(1, 1)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(512, None)));
        let mut world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        };
        // The outline is already closed by the room itself.
        assert_eq!(
            wall_gap(&world.rooms[0], [0, 0], WallEdge::North, None),
            None
        );
        assert_eq!(
            place_wall(&mut world, 0, [0, 0], WallEdge::North, None, SPEC),
            Err(WallRefusal::Covered)
        );
        // The riser fills the bottom of the lower sector's edge, so only the
        // part above it is offered.
        assert_eq!(
            wall_gap(&world.rooms[0], [1, 0], WallEdge::South, None),
            Some([512, 512, 2048, 2048])
        );
        // The platform side of that edge is not covered below its floor.
        assert_eq!(
            wall_gap(&world.rooms[0], [1, 1], WallEdge::North, None),
            Some([512, 512, 2048, 2048])
        );
        // An authored room still offers the full gap.
        world.rooms[0].wall_mode = WallMode::Authored;
        assert_eq!(
            wall_gap(&world.rooms[0], [0, 0], WallEdge::North, None),
            Some([0, 0, 2048, 2048])
        );
    }
}
