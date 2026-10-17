//! World queries over the resident rooms: locate, heights, contents, and the
//! "blocks" every trace is built from.

use crate::format::*;
use crate::height::{height_at, in_first_triangle, triangle_normal_q12, weight_recip, Recip};
use crate::view::{CollisionView, LevelView};

/// Thickness of a floor or ceiling slab, runtime units.
pub const SLAB_THICKNESS: i32 = 16;
/// Largest coordinate magnitude a query may use. Keeps every `delta * Q12`
/// product inside `i32`.
pub const COORD_LIMIT: i32 = 1 << 16;

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

/// What a blocking interval came from, for contact normals.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Source {
    Floor {
        room: usize,
        cx: usize,
        cz: usize,
        second: bool,
    },
    Ceil,
    /// Wall on `edge` of the sector whose world-plan origin is `(ox, oz)`.
    Wall {
        edge: u8,
        ox: i32,
        oz: i32,
    },
    /// A room whose collision block is not resident.
    Foreign,
}

/// A forbidden feet-height interval `(lo, hi)` (open) at a column.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Block {
    pub lo: i32,
    pub hi: i32,
    pub src: Source,
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
        })
    }

    /// Sector edge in runtime units.
    pub fn sector_units(&self) -> i32 {
        self.s
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
    fn floor_corners(view: &CollisionView<'a>, sector: &Sector, second: bool) -> [i16; 4] {
        if second && sector.flags & SECTOR_SPLIT_LEVEL != 0 {
            if let Some(b) = view.ext(sector).and_then(|e| view.split_b(e)) {
                return b.floor_b;
            }
        }
        sector.floor
    }

    #[inline]
    fn ceil_corners(view: &CollisionView<'a>, sector: &Sector, second: bool) -> [i16; 4] {
        if second && sector.flags & SECTOR_SPLIT_LEVEL != 0 {
            if let Some(b) = view.ext(sector).and_then(|e| view.split_b(e)) {
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
        view: &CollisionView<'a>,
        sector: &Sector,
        lx: u32,
        lz: u32,
    ) -> Option<(i32, bool)> {
        if sector.flags & SECTOR_HAS_FLOOR == 0 {
            return None;
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
        view: &CollisionView<'a>,
        sector: &Sector,
        lx: u32,
        lz: u32,
    ) -> Option<i32> {
        if sector.flags & SECTOR_HAS_CEIL == 0 {
            return None;
        }
        let nesw = sector.flags & SECTOR_CEIL_NESW != 0;
        let second = !in_first_triangle(nesw, lx, lz, self.s as u32);
        let corners = Self::ceil_corners(view, sector, second);
        Some(height_at(corners, nesw, lx, lz, self.s as u32, self.inv24))
    }

    /// World floor height at `(lx, lz)` inside a sector of a room.
    pub fn floor_at(&self, room: usize, cx: usize, cz: usize, lx: u32, lz: u32) -> Option<i32> {
        let view = self.view(room)?;
        let sector = view.sectors.get(cz * view.width() + cx)?;
        let (h, _) = self.sector_floor(view, sector, lx, lz)?;
        Some(h + self.level.rooms.get(room)?.origin[1])
    }

    /// World ceiling height at `(lx, lz)` inside a sector of a room.
    pub fn ceil_at(&self, room: usize, cx: usize, cz: usize, lx: u32, lz: u32) -> Option<i32> {
        let view = self.view(room)?;
        let sector = view.sectors.get(cz * view.width() + cx)?;
        let h = self.sector_ceil(view, sector, lx, lz)?;
        Some(h + self.level.rooms.get(room)?.origin[1])
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
        let corners = Self::floor_corners(view, sector, second);
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
        let mut gap = 0;
        if let Some((f, _)) = self.sector_floor(view, sector, lx, lz) {
            gap = gap.max(oy + f - y);
        }
        if let Some(c) = self.sector_ceil(view, sector, lx, lz) {
            gap = gap.max(y - (oy + c));
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
            let Some(view) = self.view(room) else {
                continue;
            };
            let rec = &self.level.rooms[room];
            let Some((cx, cz, lx, lz)) = self.cell_of(
                p[0].wrapping_sub(rec.origin[0]),
                p[2].wrapping_sub(rec.origin[2]),
                view.width(),
                view.depth(),
            ) else {
                continue;
            };
            let Some(sector) = view.sectors.get(cz * view.width() + cx) else {
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

    /// Visit every forbidden feet-height interval for a cylinder of `radius`
    /// and `height` centred at world `(x, z)`. `visit` returns true to stop;
    /// the result says whether it stopped.
    ///
    /// Slabs are sampled at the centre and, for `radius > 0`, four compass
    /// points; walls are tested against the circle. `foreign` includes rooms
    /// that are not resident.
    #[inline(never)]
    pub(crate) fn visit_blocks(
        &self,
        x: i32,
        z: i32,
        radius: i32,
        height: i32,
        foreign: bool,
        visit: &mut impl FnMut(Block) -> bool,
    ) -> bool {
        let samples = [
            (x, z),
            (x + radius, z),
            (x - radius, z),
            (x, z + radius),
            (x, z - radius),
        ];
        let count = if radius > 0 { 5 } else { 1 };
        let t = SLAB_THICKNESS;
        for (room, rec) in self.level.rooms.iter().enumerate() {
            let (ox, oy, oz) = (rec.origin[0], rec.origin[1], rec.origin[2]);
            let width = usize::from(rec.width);
            let depth = usize::from(rec.depth);
            let Some(view) = self.view(room) else {
                if foreign {
                    for &(sx, sz) in &samples[..count] {
                        if self.cell_of(sx - ox, sz - oz, width, depth).is_some() {
                            let block = Block {
                                lo: oy + i32::from(rec.min_y) - height,
                                hi: oy + i32::from(rec.max_y),
                                src: Source::Foreign,
                            };
                            if block.hi > block.lo + height && visit(block) {
                                return true;
                            }
                            break;
                        }
                    }
                }
                continue;
            };
            let (vw, vd) = (view.width(), view.depth());
            for &(sx, sz) in &samples[..count] {
                let Some((cx, cz, lx, lz)) = self.cell_of(sx - ox, sz - oz, vw, vd) else {
                    continue;
                };
                let Some(sector) = view.sectors.get(cz * vw + cx) else {
                    continue;
                };
                if let Some((f, second)) = self.sector_floor(view, sector, lx, lz) {
                    let top = oy + f;
                    let block = Block {
                        lo: top - t - height,
                        hi: top,
                        src: Source::Floor {
                            room,
                            cx,
                            cz,
                            second,
                        },
                    };
                    if visit(block) {
                        return true;
                    }
                }
                if sector.flags & SECTOR_CEIL_SOLID != 0 {
                    if let Some(c) = self.sector_ceil(view, sector, lx, lz) {
                        let bottom = oy + c;
                        let block = Block {
                            lo: bottom - height,
                            hi: bottom + t,
                            src: Source::Ceil,
                        };
                        if visit(block) {
                            return true;
                        }
                    }
                }
            }
            if radius > 0 && self.visit_walls(view, rec, x, z, radius, height, visit) {
                return true;
            }
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn visit_walls(
        &self,
        view: &CollisionView<'a>,
        rec: &RoomRecord,
        x: i32,
        z: i32,
        radius: i32,
        height: i32,
        visit: &mut impl FnMut(Block) -> bool,
    ) -> bool {
        let s = self.s;
        let (ox, oy, oz) = (rec.origin[0], rec.origin[1], rec.origin[2]);
        let (vw, vd) = (view.width() as i32, view.depth() as i32);
        let (lx, lz) = (x - ox, z - oz);
        if lx + radius < 0 || lz + radius < 0 || lx - radius >= vw * s || lz - radius >= vd * s {
            return false;
        }
        let cell = |v: i32, n: i32| -> usize {
            let v = v.clamp(0, n * s - 1) as u32;
            self.recip.cell(v) as usize
        };
        let (cx0, cx1) = (cell(lx - radius, vw), cell(lx + radius, vw));
        let (cz0, cz1) = (cell(lz - radius, vd), cell(lz + radius, vd));
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                let Some(sector) = view.sectors.get(cz * vw as usize + cx) else {
                    continue;
                };
                let Some(ext) = view.ext(sector) else {
                    continue;
                };
                let (cell_x, cell_z) = (cx as i32 * s, cz as i32 * s);
                let (px, pz) = (lx - cell_x, lz - cell_z);
                for wall in view.sector_walls(ext) {
                    if !circle_hits_wall(wall.edge, s, px, pz, radius) {
                        continue;
                    }
                    let (lo, hi) = wall_span(wall);
                    if hi <= lo {
                        continue;
                    }
                    let block = Block {
                        lo: oy + lo - height,
                        hi: oy + hi,
                        src: Source::Wall {
                            edge: wall.edge,
                            ox: ox + cell_x,
                            oz: oz + cell_z,
                        },
                    };
                    if visit(block) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// First block containing feet height `y` at the column, if any.
    #[inline(never)]
    pub(crate) fn blocking_at(
        &self,
        p: [i32; 3],
        radius: i32,
        height: i32,
        foreign: bool,
    ) -> Option<Block> {
        let mut found = None;
        self.visit_blocks(p[0], p[2], radius, height, foreign, &mut |b| {
            if b.lo < p[1] && p[1] < b.hi {
                found = Some(b);
                true
            } else {
                false
            }
        });
        found
    }

    /// True when a cylinder with feet at `p` overlaps solid.
    pub fn body_blocked(&self, p: [i32; 3], radius: i32, height: i32) -> bool {
        self.blocking_at(p, radius, height, true).is_some()
    }

    /// True when the point is strictly inside a floor or ceiling slab of a
    /// resident room.
    pub fn point_in_solid(&self, p: [i32; 3]) -> bool {
        self.blocking_at(p, 0, 0, false).is_some()
    }
}

/// Lowest bottom and highest top of a wall piece, room-local.
#[inline]
pub(crate) fn wall_span(wall: &ColWall) -> (i32, i32) {
    let [b0, b1, t1, t0] = wall.y.map(i32::from);
    (b0.min(b1), t0.max(t1))
}

/// Strict circle-against-wall-segment test in sector-local plan coordinates:
/// the segment is closer than `r` to `(px, pz)` (touching is clear).
pub(crate) fn circle_hits_wall(edge: u8, s: i32, px: i32, pz: i32, r: i32) -> bool {
    let r2 = r * r;
    match edge {
        edge::NORTH | edge::SOUTH => {
            let line = if edge == edge::NORTH { 0 } else { s };
            let cx = px.clamp(0, s);
            let (dx, dz) = (px - cx, pz - line);
            dx * dx + dz * dz < r2
        }
        edge::EAST | edge::WEST => {
            let line = if edge == edge::WEST { 0 } else { s };
            let cz = pz.clamp(0, s);
            let (dx, dz) = (px - line, pz - cz);
            dx * dx + dz * dz < r2
        }
        edge::NWSE => {
            // (0,0)->(s,s); projection along (1,1) in 0..=2s, distance q/sqrt2.
            let proj = px + pz;
            if proj < 0 {
                px * px + pz * pz < r2
            } else if proj > 2 * s {
                let (dx, dz) = (px - s, pz - s);
                dx * dx + dz * dz < r2
            } else {
                let q = px - pz;
                q * q < 2 * r2
            }
        }
        edge::NESW => {
            // (s,0)->(0,s); projection along (-1,1) maps to pz - px in -s..=s.
            let proj = pz - px;
            if proj < -s {
                let (dx, dz) = (px - s, pz);
                dx * dx + dz * dz < r2
            } else if proj > s {
                let (dx, dz) = (px, pz - s);
                dx * dx + dz * dz < r2
            } else {
                let q = px + pz - s;
                q * q < 2 * r2
            }
        }
        _ => false,
    }
}

/// Wall unit normal (Q12) flipped toward the plan point `(px, pz)` given
/// relative to the sector origin.
pub(crate) fn wall_normal_toward(edge: u8, s: i32, px: i32, pz: i32) -> [i16; 3] {
    const D: i16 = 2896;
    let pos = |v: i32| v >= 0;
    match edge {
        edge::NORTH => [0, 0, if pos(pz) { 4096 } else { -4096 }],
        edge::SOUTH => [0, 0, if pos(pz - s) { 4096 } else { -4096 }],
        edge::EAST => [if pos(px - s) { 4096 } else { -4096 }, 0, 0],
        edge::WEST => [if pos(px) { 4096 } else { -4096 }, 0, 0],
        edge::NWSE => {
            if pos(px - pz) {
                [D, 0, -D]
            } else {
                [-D, 0, D]
            }
        }
        _ => {
            if pos(px + pz - s) {
                [D, 0, D]
            } else {
                [-D, 0, -D]
            }
        }
    }
}
