//! World queries over the resident rooms: locate, heights and contents.

use super::leaf::LeafDims;
use crate::format::*;
use crate::height::{height_at, in_first_triangle, triangle_normal_q12, Recip};
use crate::lattice::{RoomsNear, MARGIN_SECTORS, SMALL_WORLD_ROOMS};
use crate::resident::ResidentRooms;
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

/// Rooms to visit, see [`GridCollision::visit`]: a lattice cell's room list,
/// or `None` for every room. One word and a length, because it is held across
/// the calls of a collision scan on a 1 KiB stack.
#[derive(Copy, Clone)]
pub(crate) struct Visit<'a>(Option<&'a [u16]>);

impl Visit<'_> {
    /// Rooms to go through, in a level of `rooms` rooms.
    #[inline(always)]
    pub(crate) fn len(&self, rooms: usize) -> usize {
        match self.0 {
            Some(list) => list.len(),
            None => rooms,
        }
    }

    /// The room at `step`, `step < len()`.
    #[inline(always)]
    pub(crate) fn room(&self, step: usize) -> usize {
        match self.0 {
            Some(list) => usize::from(list[step]),
            None => step,
        }
    }
}

/// The collision world: the level core plus the rooms resident right now
/// (see [`ResidentRooms`]).
///
/// A room that is not resident is treated as **solid over its footprint** (the
/// sectors it owns, when the core carries occupancy) for
/// bodies and as non-blocking for line of sight. No query loops over every
/// room of the level: each asks the world lattice of the core for the rooms
/// near the point, box or segment it concerns (a world of at most
/// [`SMALL_WORLD_ROOMS`] rooms just walks them all).
///
/// The value is a handful of copied constants and a reference to the set;
/// build it as often as needed with [`ResidentRooms::collision`].
#[derive(Copy, Clone)]
pub struct GridCollision<'a> {
    /// The resident set, which also holds the level core: the world keeps no
    /// copy of the core's table descriptors, so it stays a few words wide.
    pub(crate) set: &'a ResidentRooms<'a>,
    /// The room directory and the per-room collision block pointers, held
    /// here so the hot lookups cost what a plain slice index costs.
    pub(crate) rooms: &'a [RoomRecord],
    pub(crate) views: &'a [*const CollisionView<'a>],
    pub(crate) s: i32,
    pub(crate) recip: Recip,
    pub(crate) inv24: u32,
    /// Edge and weight reciprocal of every leaf size code (tree rooms).
    pub(crate) leaf: &'a LeafDims,
    /// Some room's collision block is not resident.
    pub(crate) foreign: bool,
    /// Some resident room holds collidable free planes, so the plane scans
    /// are worth running.
    pub(crate) planes: bool,
    /// Find nearby rooms through the world lattice instead of walking every
    /// room (set for worlds beyond [`SMALL_WORLD_ROOMS`]; tests force it).
    pub(crate) lattice: bool,
    /// Validate analytic point-trace results against the discrete slab
    /// predicate even when every room is resident (tests compare both).
    pub(crate) validate: bool,
    /// The trace is the camera arm: planes flagged
    /// [`crate::COLPLANE_CAMERA_PASS`] are not there.
    pub(crate) camera_pass: bool,
}

impl<'a> GridCollision<'a> {
    /// The world over the rooms resident in `set` now.
    pub(crate) fn over(set: &'a ResidentRooms<'a>) -> Self {
        let level = set.level();
        Self {
            set,
            rooms: level.rooms,
            views: set.view_table(),
            s: set.s,
            recip: set.recip,
            inv24: set.inv24,
            leaf: &set.leaf,
            foreign: !set.is_complete(),
            planes: set.has_planes(),
            lattice: level.rooms.len() > SMALL_WORLD_ROOMS,
            validate: false,
            camera_pass: false,
        }
    }

    /// The level core.
    #[inline(always)]
    pub(crate) fn level(&self) -> &'a LevelView<'a> {
        self.set.level()
    }

    /// The same world with lattice lookups forced on or off, for tests that
    /// prove both ways agree.
    pub fn with_lattice(mut self, on: bool) -> Self {
        self.lattice = on;
        self
    }

    /// The rooms a query centred on `(x, z)` with half-extent `r` must
    /// visit: the room list of the one lattice cell holding the centre when
    /// that list is guaranteed to cover the box (`r` within the lattice
    /// margin), every room otherwise (a world that skips the lattice, an
    /// unusually wide body). Two words of state, no iterator.
    #[inline(always)]
    pub(crate) fn visit(&self, x: i32, z: i32, r: i32) -> Visit<'a> {
        if self.lattice && r <= MARGIN_SECTORS * self.s {
            Visit(Some(self.level().lattice_rooms_at(x, z)))
        } else {
            Visit(None)
        }
    }

    /// Rooms whose footprint may touch the inclusive plan box
    /// `[lo.0, hi.0] x [lo.1, hi.1]`, world runtime units.
    #[inline(always)]
    pub(crate) fn rooms_near(&self, lo: (i32, i32), hi: (i32, i32)) -> RoomsNear<'a> {
        if self.lattice {
            RoomsNear::Lattice(self.level().lattice().candidates(self.level(), lo, hi))
        } else {
            RoomsNear::All(0..self.rooms.len() as u32)
        }
    }

    /// Sector edge in runtime units.
    pub fn sector_units(&self) -> i32 {
        self.s
    }

    /// Room record, or an empty one for an out-of-range index (a panic path
    /// would cost every caller a stack frame).
    #[inline(always)]
    pub(crate) fn rec(&self, room: usize) -> &RoomRecord {
        self.rooms.get(room).unwrap_or(&NO_ROOM)
    }

    #[inline]
    pub(crate) fn view(&self, room: usize) -> Option<&CollisionView<'a>> {
        let view = *self.views.get(room)?;
        // SAFETY: see `ResidentRooms::collision_of`; `views` is that table
        // and the world's borrow of the set keeps it unchanged.
        unsafe { view.as_ref() }
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
        // A leaf of another size has its own edge and reciprocal; a whole
        // sector (every record of a dense room) reads the level's.
        let (s, inv) = self.dims_of(sector.flags);
        let second = !in_first_triangle(nesw, lx, lz, s);
        let corners = Self::floor_corners(view, sector, second);
        Some((height_at(corners, nesw, lx, lz, s, inv), second))
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
        let (s, inv) = self.dims_of(sector.flags);
        let second = !in_first_triangle(nesw, lx, lz, s);
        let corners = Self::ceil_corners(view, sector, second);
        Some(height_at(corners, nesw, lx, lz, s, inv))
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

    /// World floor height at `(lx, lz)` inside a sector of a room: the
    /// column's ground span (its lowest). See [`Self::floor_below`] for a
    /// column of several spans.
    pub fn floor_at(&self, room: usize, cx: usize, cz: usize, lx: u32, lz: u32) -> Option<i32> {
        let view = self.view(room)?;
        let (sector, lx, lz, _) = self.leaf_local(view, cx, cz, lx, lz)?;
        let h = if sector.flags & SECTOR_FLAT_FLOOR != 0 {
            i32::from(sector.floor[0])
        } else {
            self.sector_floor(Some(view), sector, lx, lz)?.0
        };
        Some(h + self.rec(room).origin[1])
    }

    /// World ceiling height at `(lx, lz)` inside a sector of a room: the
    /// column's ground span (its lowest). See [`Self::ceil_above`] for a
    /// column of several spans.
    pub fn ceil_at(&self, room: usize, cx: usize, cz: usize, lx: u32, lz: u32) -> Option<i32> {
        let view = self.view(room)?;
        let (sector, lx, lz, _) = self.leaf_local(view, cx, cz, lx, lz)?;
        let h = if sector.flags & SECTOR_FLAT_CEIL != 0 {
            i32::from(sector.ceil[0])
        } else {
            self.sector_ceil(Some(view), sector, lx, lz)?
        };
        Some(h + self.rec(room).origin[1])
    }

    /// World height of the highest floor of a column at `(lx, lz)` inside a
    /// sector of a room that is at or below `y`: the floor a body at height
    /// `y` stands on or falls onto, over every span of the column. `None`
    /// when no span of the column has a floor that low. [`Self::floor_at`]
    /// is the ground span's floor only.
    pub fn floor_below(
        &self,
        room: usize,
        (cx, cz): (usize, usize),
        (lx, lz): (u32, u32),
        y: i32,
    ) -> Option<i32> {
        let view = self.view(room)?;
        let (sector, lx, lz, record) = self.leaf_local(view, cx, cz, lx, lz)?;
        let oy = self.rec(room).origin[1];
        let mut best: Option<i32> = None;
        let extras = view.extra_spans(record);
        for span in core::iter::once(sector).chain(extras) {
            if let Some((f, _)) = self.sector_floor(Some(view), span, lx, lz) {
                let h = oy + f;
                if h <= y && best.is_none_or(|b| h > b) {
                    best = Some(h);
                }
            }
        }
        best
    }

    /// World height of the lowest ceiling of a column at `(lx, lz)` inside a
    /// sector of a room that is at or above `y`, over every span of the
    /// column; `None` when none is. [`Self::ceil_at`] is the ground span's
    /// ceiling only.
    pub fn ceil_above(
        &self,
        room: usize,
        (cx, cz): (usize, usize),
        (lx, lz): (u32, u32),
        y: i32,
    ) -> Option<i32> {
        let view = self.view(room)?;
        let (sector, lx, lz, record) = self.leaf_local(view, cx, cz, lx, lz)?;
        let oy = self.rec(room).origin[1];
        let mut best: Option<i32> = None;
        let extras = view.extra_spans(record);
        for span in core::iter::once(sector).chain(extras) {
            if let Some(c) = self.sector_ceil(Some(view), span, lx, lz) {
                let h = oy + c;
                if h >= y && best.is_none_or(|b| h < b) {
                    best = Some(h);
                }
            }
        }
        best
    }

    /// World floor height at the sector centre of a column, `None` for a pit.
    pub fn column_floor(&self, room: usize, cx: usize, cz: usize) -> Option<i32> {
        let half = (self.s / 2) as u32;
        self.floor_at(room, cx, cz, half, half)
    }

    /// Upward-facing normal of the floor triangle a slab hit refers to: the
    /// source names the room, the sector (`cx`, `cz`), the quarter sector in
    /// a divided one (`aux >> 3`) and the triangle (`aux & 1`).
    pub(crate) fn floor_normal(&self, src: &super::scan::Src) -> [i16; 3] {
        let up = [0, 4096, 0];
        let Some(view) = self.view(usize::from(src.room)) else {
            return up;
        };
        let (cx, cz) = (usize::from(src.cx), usize::from(src.cz));
        let (mut sector, record) = if view.is_tree() {
            let leaf = self.leaf_named(view, cx, cz, src.aux >> 3);
            (leaf.rec, usize::from(leaf.id))
        } else {
            let record = cz * view.width() + cx;
            let Some(sector) = view.sectors.get(record) else {
                return up;
            };
            (sector, record)
        };
        if src.span != 0 {
            // The hit is a slab of an extra span: its own triangle.
            match view.extra_spans(record).get(usize::from(src.span) - 1) {
                Some(span) => sector = span,
                None => return up,
            }
        }
        let second = src.aux & 1 != 0;
        let nesw = sector.flags & SECTOR_FLOOR_NESW != 0;
        let corners = Self::floor_corners(Some(view), sector, second);
        triangle_normal_q12(corners, nesw, second, self.dims_of(sector.flags).0)
    }

    /// The record that holds sector `(cx, cz)`'s offset `(lx, lz)`, the
    /// offsets in that record's frame and the record's index in `SECTORS`
    /// (the key of its extra spans): the sector and the same offsets in a
    /// dense room, the leaf and offsets from its origin in a tree room.
    #[inline(always)]
    pub(crate) fn leaf_local<'v>(
        &self,
        view: &'v CollisionView<'_>,
        cx: usize,
        cz: usize,
        lx: u32,
        lz: u32,
    ) -> Option<(&'v Sector, u32, u32, usize)> {
        if !view.is_tree() {
            let index = cz * view.width() + cx;
            return view.sectors.get(index).map(|s| (s, lx, lz, index));
        }
        let leaf = self.leaf_in(view, cx, cz, lx as i32, lz as i32);
        let s = self.s;
        Some((
            leaf.rec,
            (lx as i32 + cx as i32 * s - leaf.ox).max(0) as u32,
            (lz as i32 + cz as i32 * s - leaf.oz).max(0) as u32,
            usize::from(leaf.id),
        ))
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

    /// Vertical distance from `y` to the nearest span of a column: its ground
    /// span (`sector`, record `record`) or any of its extra spans. Zero when
    /// `y` is inside one of them.
    #[inline(never)]
    fn column_gap_all(
        &self,
        view: &CollisionView<'a>,
        sector: &Sector,
        record: usize,
        oy: i32,
        (lx, lz): (u32, u32),
        y: i32,
    ) -> i32 {
        let mut gap = self.column_gap(view, sector, oy, lx, lz, y);
        if gap == 0 || view.span_index.is_empty() {
            return gap;
        }
        for span in view.extra_spans(record) {
            gap = gap.min(self.column_gap(view, span, oy, lx, lz, y));
            if gap == 0 {
                break;
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
    ///
    /// A column of several spans fits when the point is inside any of its
    /// spans, and its distance is that to the nearest span: the answer is
    /// the same sector whichever span the point is in (the result names no
    /// span; a caller that needs it asks [`Self::floor_below`] or
    /// [`Self::ceil_above`] at the point's height).
    #[inline(never)]
    pub fn locate(&self, hint_room: usize, p: [i32; 3]) -> Option<Located> {
        let count = self.rooms.len();
        let mut best: Option<(i32, Located)> = None;
        let near = self.visit(p[0], p[2], 0);
        for step in 0..=near.len(count) {
            let room = if step == 0 {
                hint_room
            } else {
                near.room(step - 1)
            };
            if room >= count || (step > 0 && room == hint_room) {
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
            if !self.level().room_owns_cell(room, cx, cz) {
                continue;
            }
            let Some(view) = self.view(room) else {
                continue;
            };
            let Some((sector, llx, llz, record)) = self.leaf_local(view, cx, cz, lx, lz) else {
                continue;
            };
            let gap = self.column_gap_all(view, sector, record, rec.origin[1], (llx, llz), p[1]);
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
        let near = self.visit(p[0], p[2], 0);
        for step in 0..near.len(self.rooms.len()) {
            let room = near.room(step);
            let rec = self.rec(room);
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

/// Strict circle-against-wall-segment test in leaf-local plan coordinates:
/// the segment is closer than `r` to `(px, pz)` (touching is clear). `s` is
/// the edge of the leaf that owns the wall and `(lo, hi)` the part of a
/// cardinal edge the wall covers along it (`(0, s)` for the whole edge).
#[inline(always)]
pub(crate) fn circle_hits_wall(
    edge: u8,
    s: i32,
    px: i32,
    pz: i32,
    r: i32,
    (lo, hi): (i32, i32),
) -> bool {
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
        let du = u - u.max(lo).min(hi);
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

/// The part of its owner's edge a wall covers, in plan coordinates along the
/// edge (increasing X for north and south, increasing Z for east and west):
/// the whole `0..s`, or one half of it for a [`WALL_HALF_FIRST`] or
/// [`WALL_HALF_SECOND`] wall (first means from the edge's start `a` in its
/// walking direction, which is the upper half for south and west).
#[inline(always)]
pub(crate) fn wall_range(wall: &ColWall, s: i32) -> (i32, i32) {
    let halves = wall.flags & (WALL_HALF_FIRST | WALL_HALF_SECOND);
    if halves == 0 {
        return (0, s);
    }
    let forward = wall.edge == edge::NORTH || wall.edge == edge::EAST;
    let low = (halves == WALL_HALF_FIRST) == forward;
    if low {
        (0, s / 2)
    } else {
        (s / 2, s)
    }
}

/// Whether the plan point `(px, pz)`, relative to the sector origin, is on the
/// front side of a wall on `edge`: the interior of a cardinal edge, the NE
/// side of the NW-SE diagonal, the NW side of the NE-SW diagonal. The wall's
/// own line counts as the front.
#[inline(always)]
pub(crate) fn on_front(edge: u8, s: i32, px: i32, pz: i32) -> bool {
    match edge {
        edge::NORTH => pz >= 0,
        edge::SOUTH => pz <= s,
        edge::EAST => px <= s,
        edge::WEST => px >= 0,
        edge::NWSE => px >= pz,
        _ => px + pz <= s,
    }
}

/// Whether a wall's one-sided flags let a body or ray at `(px, pz)` be
/// stopped by it.
#[inline(always)]
pub(crate) fn wall_applies(wall: &ColWall, s: i32, px: i32, pz: i32) -> bool {
    let sided = wall.flags & (WALL_FRONT_ONLY | WALL_BACK_ONLY);
    sided == 0 || on_front(wall.edge, s, px, pz) == (sided & WALL_FRONT_ONLY != 0)
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
