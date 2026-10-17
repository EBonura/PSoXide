//! World queries over the resident rooms: locate, heights and contents.

use crate::format::*;
use crate::height::{height_at, in_first_triangle, triangle_normal_q12, weight_recip, Recip};
use crate::view::{CollisionView, LevelView};

/// Thickness of a floor or ceiling slab, runtime units.
pub const SLAB_THICKNESS: i32 = 16;
/// Largest coordinate magnitude a query may use. Keeps every `delta * Q12`
/// product inside `i32`.
pub const COORD_LIMIT: i32 = 1 << 16;

/// The record `rec` falls back to.
static NO_ROOM: RoomRecord = RoomRecord {
    pack_sector: 0,
    pack_sectors: 0,
    flags: 0,
    origin: [0; 3],
    width: 0,
    depth: 0,
    portal_first: 0,
    portal_count: 0,
    min_y: 0,
    max_y: 0,
    fog_near: 0,
    fog_far: 0,
    fog_rgb: [0; 3],
    reserved: [0; 3],
};

/// A world point resolved to a room and sector.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub room: usize,
    pub cx: usize,
    pub cz: usize,
    /// Offset inside the sector along X, `0..S`.
    pub lx: u32,
    /// Offset inside the sector along Z, `0..S`.
    pub lz: u32,
}

/// The collision world: the level core plus the resident room collision
/// blocks.
///
/// `rooms[i]` is room `i`'s collision block, `None` when it is not resident.
/// A missing room is treated as **solid over its footprint** for bodies and as
/// non-blocking for line of sight.
#[derive(Copy, Clone)]
pub struct GridCollision<'a> {
    pub(crate) level: LevelView<'a>,
    pub(crate) rooms: &'a [Option<CollisionView<'a>>],
    pub(crate) s: i32,
    pub(crate) recip: Recip,
    pub(crate) inv24: u32,
    /// Some room's collision block is not resident.
    pub(crate) foreign: bool,
    /// Some resident room holds collidable free planes, so the plane scans
    /// are worth running.
    pub(crate) planes: bool,
    /// Validate analytic point-trace results against the discrete slab
    /// predicate even when every room is resident (tests compare both).
    pub(crate) validate: bool,
}

impl<'a> GridCollision<'a> {
    /// Build the world. `None` when no exact cell reciprocal exists for the
    /// level's sector size.
    pub fn new(level: LevelView<'a>, rooms: &'a [Option<CollisionView<'a>>]) -> Option<Self> {
        let s = level.sector_units();
        let recip = Recip::for_sector(s, 255 * s)?;
        Some(Self {
            level,
            rooms,
            s: s as i32,
            recip,
            inv24: weight_recip(s),
            foreign: rooms.iter().any(Option::is_none) || rooms.len() < level.rooms.len(),
            planes: rooms.iter().flatten().any(|v| !v.col_planes.is_empty()),
            validate: false,
        })
    }

    /// Sector edge in runtime units.
    pub fn sector_units(&self) -> i32 {
        self.s
    }

    /// Room record, or an empty one for an out-of-range index (a panic path
    /// would cost every caller a stack frame).
    #[inline(always)]
    pub(crate) fn rec(&self, room: usize) -> &RoomRecord {
        self.level.rooms.get(room).unwrap_or(&NO_ROOM)
    }

    #[inline]
    pub(crate) fn view(&self, room: usize) -> Option<&CollisionView<'a>> {
        self.rooms.get(room).and_then(|r| r.as_ref())
    }

    /// Plan cell of room-local `(x, z)`: `(cx, cz, lx, lz)`.
    #[inline]
    pub(crate) fn cell_of(
        &self,
        x: i32,
        z: i32,
        width: usize,
        depth: usize,
    ) -> Option<(usize, usize, u32, u32)> {
        if x < 0 || z < 0 {
            return None;
        }
        let (ux, uz) = (x as u32, z as u32);
        let s = self.s as u32;
        if ux >= width as u32 * s || uz >= depth as u32 * s {
            return None;
        }
        let cx = self.recip.cell(ux);
        let cz = self.recip.cell(uz);
        Some((cx as usize, cz as usize, ux - cx * s, uz - cz * s))
    }

    #[inline]
    fn floor_corners(view: Option<&CollisionView<'a>>, sector: &Sector, second: bool) -> [i16; 4] {
        if second && sector.flags & SECTOR_SPLIT_LEVEL != 0 {
            if let Some(b) = view.and_then(|v| v.ext(sector).and_then(|e| v.split_b(e))) {
                return b.floor_b;
            }
        }
        sector.floor
    }

    #[inline]
    fn ceil_corners(view: Option<&CollisionView<'a>>, sector: &Sector, second: bool) -> [i16; 4] {
        if second && sector.flags & SECTOR_SPLIT_LEVEL != 0 {
            if let Some(b) = view.and_then(|v| v.ext(sector).and_then(|e| v.split_b(e))) {
                return b.ceil_b;
            }
        }
        sector.ceil
    }

    /// Room-local floor height of a sector at `(lx, lz)` and whether the point
    /// is in the second triangle. `None` for a pit.
    #[inline]
    pub(crate) fn sector_floor(
        &self,
        view: Option<&CollisionView<'a>>,
        sector: &Sector,
        lx: u32,
        lz: u32,
    ) -> Option<(i32, bool)> {
        if sector.flags & SECTOR_HAS_FLOOR == 0 {
            return None;
        }
        if sector.flags & SECTOR_FLAT_FLOOR != 0 {
            // Both triangles share the plane; the triangle flag is moot.
            return Some((i32::from(sector.floor[0]), false));
        }
        let nesw = sector.flags & SECTOR_FLOOR_NESW != 0;
        let second = !in_first_triangle(nesw, lx, lz, self.s as u32);
        let corners = Self::floor_corners(view, sector, second);
        Some((
            height_at(corners, nesw, lx, lz, self.s as u32, self.inv24),
            second,
        ))
    }

    /// Room-local ceiling height; `None` without a ceiling. Solidity is a
    /// separate flag.
    #[inline]
    pub(crate) fn sector_ceil(
        &self,
        view: Option<&CollisionView<'a>>,
        sector: &Sector,
        lx: u32,
        lz: u32,
    ) -> Option<i32> {
        if sector.flags & SECTOR_HAS_CEIL == 0 {
            return None;
        }
        if sector.flags & SECTOR_FLAT_CEIL != 0 {
            return Some(i32::from(sector.ceil[0]));
        }
        let nesw = sector.flags & SECTOR_CEIL_NESW != 0;
        let second = !in_first_triangle(nesw, lx, lz, self.s as u32);
        let corners = Self::ceil_corners(view, sector, second);
        Some(height_at(corners, nesw, lx, lz, self.s as u32, self.inv24))
    }

    /// Lowest and highest room-local floor corner of a sector (both triangles
    /// of a split-level sector). Every interpolated height lies between them.
    #[inline]
    pub(crate) fn floor_range(
        &self,
        view: Option<&CollisionView<'a>>,
        sector: &Sector,
    ) -> (i32, i32) {
        if sector.flags & SECTOR_FLAT_FLOOR != 0 {
            return (i32::from(sector.floor[0]), i32::from(sector.floor[0]));
        }
        let (mut lo, mut hi) = corner_range(sector.floor);
        if sector.flags & SECTOR_SPLIT_LEVEL != 0 {
            let (l, h) = corner_range(Self::floor_corners(view, sector, true));
            lo = lo.min(l);
            hi = hi.max(h);
        }
        (lo, hi)
    }

    /// Lowest and highest room-local ceiling corner of a sector.
    #[inline]
    pub(crate) fn ceil_range(
        &self,
        view: Option<&CollisionView<'a>>,
        sector: &Sector,
    ) -> (i32, i32) {
        if sector.flags & SECTOR_FLAT_CEIL != 0 {
            return (i32::from(sector.ceil[0]), i32::from(sector.ceil[0]));
        }
        let (mut lo, mut hi) = corner_range(sector.ceil);
        if sector.flags & SECTOR_SPLIT_LEVEL != 0 {
            let (l, h) = corner_range(Self::ceil_corners(view, sector, true));
            lo = lo.min(l);
            hi = hi.max(h);
        }
        (lo, hi)
    }

    /// World floor height at `(lx, lz)` inside a sector of a room.
    pub fn floor_at(&self, room: usize, cx: usize, cz: usize, lx: u32, lz: u32) -> Option<i32> {
        let view = self.view(room)?;
        let sector = view.sectors.get(cz * view.width() + cx)?;
        let h = if sector.flags & SECTOR_FLAT_FLOOR != 0 {
            i32::from(sector.floor[0])
        } else {
            self.sector_floor(Some(view), sector, lx, lz)?.0
        };
        Some(h + self.rec(room).origin[1])
    }

    /// World ceiling height at `(lx, lz)` inside a sector of a room.
    pub fn ceil_at(&self, room: usize, cx: usize, cz: usize, lx: u32, lz: u32) -> Option<i32> {
        let view = self.view(room)?;
        let sector = view.sectors.get(cz * view.width() + cx)?;
        let h = if sector.flags & SECTOR_FLAT_CEIL != 0 {
            i32::from(sector.ceil[0])
        } else {
            self.sector_ceil(Some(view), sector, lx, lz)?
        };
        Some(h + self.rec(room).origin[1])
    }

    /// World floor height at the sector centre of a column, `None` for a pit.
    pub fn column_floor(&self, room: usize, cx: usize, cz: usize) -> Option<i32> {
        let half = (self.s / 2) as u32;
        self.floor_at(room, cx, cz, half, half)
    }

    /// Upward-facing normal of the floor triangle a slab hit refers to.
    pub(crate) fn floor_normal(&self, room: usize, cx: usize, cz: usize, second: bool) -> [i16; 3] {
        let up = [0, 4096, 0];
        let Some(view) = self.view(room) else {
            return up;
        };
        let Some(sector) = view.sectors.get(cz * view.width() + cx) else {
            return up;
        };
        let nesw = sector.flags & SECTOR_FLOOR_NESW != 0;
        let corners = Self::floor_corners(Some(view), sector, second);
        triangle_normal_q12(corners, nesw, second, self.s as u32)
    }

    /// Vertical distance from `y` to the span of a column, 0 when inside it.
    fn column_gap(
        &self,
        view: &CollisionView<'a>,
        sector: &Sector,
        oy: i32,
        lx: u32,
        lz: u32,
        y: i32,
    ) -> i32 {
        let flags = sector.flags;
        let mut gap = 0;
        // Flat slabs (the common case) need no interpolation, and a missing
        // slab needs no call at all.
        if flags & SECTOR_HAS_FLOOR != 0 {
            if flags & SECTOR_FLAT_FLOOR != 0 {
                gap = gap.max(oy + i32::from(sector.floor[0]) - y);
            } else if let Some((f, _)) = self.sector_floor(Some(view), sector, lx, lz) {
                gap = gap.max(oy + f - y);
            }
        }
        if flags & SECTOR_HAS_CEIL != 0 {
            if flags & SECTOR_FLAT_CEIL != 0 {
                gap = gap.max(y - (oy + i32::from(sector.ceil[0])));
            } else if let Some(c) = self.sector_ceil(Some(view), sector, lx, lz) {
                gap = gap.max(y - (oy + c));
            }
        }
        gap
    }

    /// Resolve a world point to a room and sector.
    ///
    /// The hint room is tried first. A room fits when the point is inside its
    /// plan footprint and not below the column's floor or above its ceiling.
    /// Stacked rooms over one column are told apart that way. When no room
    /// fits, the footprint room whose column is vertically nearest wins, so a
    /// point just inside a floor or ceiling slab still has a room.
    #[inline(never)]
    pub fn locate(&self, hint_room: usize, p: [i32; 3]) -> Option<Located> {
        let count = self.level.rooms.len();
        let mut best: Option<(i32, Located)> = None;
        for step in 0..count {
            let room = if step == 0 {
                hint_room
            } else if step - 1 < hint_room {
                step - 1
            } else {
                step
            };
            if room >= count {
                continue;
            }
            let rec = self.rec(room);
            let (w, d) = (usize::from(rec.width), usize::from(rec.depth));
            // Footprint first: it needs only the room record.
            let Some((cx, cz, lx, lz)) = self.cell_of(
                p[0].wrapping_sub(rec.origin[0]),
                p[2].wrapping_sub(rec.origin[2]),
                w,
                d,
            ) else {
                continue;
            };
            let Some(view) = self.view(room) else {
                continue;
            };
            let Some(sector) = view.sectors.get(cz * w + cx) else {
                continue;
            };
            let gap = self.column_gap(view, sector, rec.origin[1], lx, lz, p[1]);
            let found = Located {
                room,
                cx,
                cz,
                lx,
                lz,
            };
            if gap == 0 {
                return Some(found);
            }
            if best.is_none_or(|(g, _)| gap < g) {
                best = Some((gap, found));
            }
        }
        best.map(|(_, l)| l)
    }

    /// Liquid contents at a world point: 0 empty, else a `liquid_kind` code.
    pub fn contents_at(&self, p: [i32; 3]) -> u8 {
        for (room, rec) in self.level.rooms.iter().enumerate() {
            let Some(view) = self.view(room) else {
                continue;
            };
            if view.liquids.is_empty() {
                continue;
            }
            let Some((cx, cz, _, _)) = self.cell_of(
                p[0].wrapping_sub(rec.origin[0]),
                p[2].wrapping_sub(rec.origin[2]),
                view.width(),
                view.depth(),
            ) else {
                continue;
            };
            let oy = rec.origin[1];
            for liquid in view.liquids {
                if (usize::from(liquid.x0)..=usize::from(liquid.x1)).contains(&cx)
                    && (usize::from(liquid.z0)..=usize::from(liquid.z1)).contains(&cz)
                    && oy + i32::from(liquid.bottom_y) <= p[1]
                    && p[1] <= oy + i32::from(liquid.surface_y)
                {
                    return liquid.kind;
                }
            }
        }
        0
    }
}

/// Lowest and highest of four corner heights.
#[inline(always)]
fn corner_range(h: [i16; 4]) -> (i32, i32) {
    let lo = h[0].min(h[1]).min(h[2]).min(h[3]);
    let hi = h[0].max(h[1]).max(h[2]).max(h[3]);
    (i32::from(lo), i32::from(hi))
}

/// Lowest bottom and highest top of a wall piece, room-local.
#[inline]
pub(crate) fn wall_span(wall: &ColWall) -> (i32, i32) {
    let [b0, b1, t1, t0] = wall.y.map(i32::from);
    (b0.min(b1), t0.max(t1))
}

/// Strict circle-against-wall-segment test in sector-local plan coordinates:
/// the segment is closer than `r` to `(px, pz)` (touching is clear).
#[inline(always)]
pub(crate) fn circle_hits_wall(edge: u8, s: i32, px: i32, pz: i32, r: i32) -> bool {
    let r2 = r * r;
    if edge < edge::NWSE {
        // Axis edges: N/S run along X at z = 0/s, E/W along Z at x = s/0.
        let along_x = edge & 1 == 0;
        let (u, v) = if along_x { (px, pz) } else { (pz, px) };
        let line = if edge == edge::NORTH || edge == edge::WEST {
            0
        } else {
            s
        };
        let du = u - u.max(0).min(s);
        let dv = v - line;
        return du * du + dv * dv < r2;
    }
    // Diagonals: `t` runs along the segment, `q` is sqrt(2) times the
    // distance from its line.
    let (t, q, end0, end1) = if edge == edge::NWSE {
        (px + pz, px - pz, (0, 0), (s, s))
    } else {
        (pz - px + s, px + pz - s, (s, 0), (0, s))
    };
    if t < 0 {
        let (dx, dz) = (px - end0.0, pz - end0.1);
        dx * dx + dz * dz < r2
    } else if t > 2 * s {
        let (dx, dz) = (px - end1.0, pz - end1.1);
        dx * dx + dz * dz < r2
    } else {
        q * q < 2 * r2
    }
}

/// Normal code of a wall flipped toward the plan point `(px, pz)` given
/// relative to the sector origin.
pub(crate) fn wall_code(edge: u8, s: i32, px: i32, pz: i32) -> u8 {
    use super::scan::nc;
    let pos = |v: i32| v >= 0;
    match edge {
        edge::NORTH => {
            if pos(pz) {
                nc::PZ
            } else {
                nc::NZ
            }
        }
        edge::SOUTH => {
            if pos(pz - s) {
                nc::PZ
            } else {
                nc::NZ
            }
        }
        edge::EAST => {
            if pos(px - s) {
                nc::PX
            } else {
                nc::NX
            }
        }
        edge::WEST => {
            if pos(px) {
                nc::PX
            } else {
                nc::NX
            }
        }
        edge::NWSE => {
            if pos(px - pz) {
                nc::PN
            } else {
                nc::NP
            }
        }
        _ => {
            if pos(px + pz - s) {
                nc::PP
            } else {
                nc::NN
            }
        }
    }
}

/// [`GridCollision::cell_of`] over already-loaded constants (the reciprocal,
/// `s`, and the room extent `ext_x`, `ext_z` in units), for loops that would
/// otherwise reload them from RAM at every sample.
#[inline(always)]
pub(crate) fn cell_in(
    recip: &Recip,
    s: u32,
    (x, z): (i32, i32),
    (ext_x, ext_z): (u32, u32),
) -> Option<(usize, usize, u32, u32)> {
    if x < 0 || z < 0 {
        return None;
    }
    let (ux, uz) = (x as u32, z as u32);
    if ux >= ext_x || uz >= ext_z {
        return None;
    }
    let cx = recip.cell(ux);
    let cz = recip.cell(uz);
    Some((cx as usize, cz as usize, ux - cx * s, uz - cz * s))
}
