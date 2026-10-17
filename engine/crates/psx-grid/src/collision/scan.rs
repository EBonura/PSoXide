//! Column scans: the forbidden feet-height intervals every body trace is
//! built from.
//!
//! The scan is deliberately non-generic and writes into a small [`Scan`]
//! accumulator owned by the caller, so each function in the chain keeps a
//! frame of a few dozen bytes (the motor runs the provider on a 1 KiB stack).

use super::world::{wall_span, GridCollision, SLAB_THICKNESS};
use crate::format::*;
use crate::view::CollisionView;

/// Normal codes. Expanded to Q12 vectors only at the provider boundary.
pub(crate) mod nc {
    pub const NONE: u8 = 0;
    pub const UP: u8 = 1;
    pub const DOWN: u8 = 2;
    pub const PX: u8 = 3;
    pub const NX: u8 = 4;
    pub const PZ: u8 = 5;
    pub const NZ: u8 = 6;
    pub const PP: u8 = 7;
    pub const PN: u8 = 8;
    pub const NP: u8 = 9;
    pub const NN: u8 = 10;
    /// Upward triangle normal of the floor named by the hit's [`Src`].
    pub const FLOOR: u8 = 11;
}

/// Source kinds.
pub(crate) mod kind {
    pub const FLOOR: u8 = 1;
    pub const CEIL: u8 = 2;
    pub const WALL: u8 = 3;
    pub const FOREIGN: u8 = 4;
}

/// What a blocking interval came from: a floor triangle (`aux` = second
/// triangle), a ceiling, a wall (`aux` = edge) or a non-resident room.
#[derive(Copy, Clone, Default)]
pub(crate) struct Src {
    pub kind: u8,
    pub aux: u8,
    pub cx: u8,
    pub cz: u8,
    pub room: u16,
}

pub(crate) const ANY: u8 = 0;
pub(crate) const SWEEP: u8 = 1;

/// Accumulator and query for one column scan: the cylinder (`x`, `z`, `r`,
/// `h`) and the interval hits.
///
/// `ANY` stops at the first interval containing `y0`. `SWEEP` collects, for a
/// vertical move `y0 -> y1`, whether either end is inside and the first
/// interval met along the way. Keeping the query inside the accumulator keeps
/// every scan function down to a couple of arguments.
#[derive(Copy, Clone)]
pub(crate) struct Scan {
    pub x: i32,
    pub z: i32,
    pub r: i32,
    pub y0: i32,
    pub y1: i32,
    pub h: i32,
    pub lo: i32,
    pub hi: i32,
    pub src: Src,
    pub mode: u8,
    pub foreign: bool,
    pub start_in: bool,
    pub end_in: bool,
    pub found: bool,
    /// Wall scan: the cell being tested and the circle centre relative to it.
    pub wcx: u8,
    pub wcz: u8,
    pub wx0: u8,
    pub wx1: u8,
    pub wz1: u8,
    pub px: i32,
    pub pz: i32,
}

impl Scan {
    pub fn any(p: [i32; 3], r: i32, h: i32, foreign: bool) -> Self {
        Self {
            x: p[0],
            z: p[2],
            r,
            y0: p[1],
            y1: p[1],
            h,
            lo: 0,
            hi: 0,
            src: Src::default(),
            mode: ANY,
            foreign,
            start_in: false,
            end_in: false,
            found: false,
            wcx: 0,
            wcz: 0,
            wx0: 0,
            wx1: 0,
            wz1: 0,
            px: 0,
            pz: 0,
        }
    }

    pub fn sweep(p: [i32; 3], y1: i32, r: i32, h: i32, foreign: bool) -> Self {
        Self {
            y1,
            mode: SWEEP,
            ..Self::any(p, r, h, foreign)
        }
    }

    #[inline(always)]
    fn done(&self) -> bool {
        self.mode == ANY && self.found
    }

    #[inline(always)]
    fn take(&mut self, lo: i32, hi: i32, src: Src) {
        self.found = true;
        self.lo = lo;
        self.hi = hi;
        self.src = src;
    }

    /// Offer the open feet-height interval `(lo, hi)`.
    #[inline(never)]
    fn offer(&mut self, lo: i32, hi: i32, src: Src) {
        let (y0, y1) = (self.y0, self.y1);
        if self.mode == ANY {
            if lo < y0 && y0 < hi && !self.found {
                self.take(lo, hi, src);
            }
            return;
        }
        self.start_in |= lo < y0 && y0 < hi;
        self.end_in |= lo < y1 && y1 < hi;
        if y1 < y0 {
            if hi <= y0 && hi > y1 && (!self.found || hi > self.hi) {
                self.take(lo, hi, src);
            }
        } else if y1 > y0 && lo >= y0 && lo < y1 && (!self.found || lo < self.lo) {
            self.take(lo, hi, src);
        }
    }

    /// Is the wall on `edge` of the current cell closer than `r` to the
    /// circle centre (strictly; touching is clear)?
    #[inline(never)]
    pub fn hits_wall(&self, edge: u8, s: i32) -> bool {
        super::world::circle_hits_wall(edge, s, self.px, self.pz, self.r)
    }

    /// Compass sample `i` of the cylinder footprint.
    #[inline(always)]
    fn sample(&self, i: i32) -> (i32, i32) {
        let (x, z, r) = (self.x, self.z, self.r);
        match i {
            0 => (x, z),
            1 => (x + r, z),
            2 => (x - r, z),
            3 => (x, z + r),
            _ => (x, z - r),
        }
    }
}

impl<'a> GridCollision<'a> {
    /// Scan every room for intervals at the scan's column.
    #[inline(always)]
    pub(crate) fn scan_column(&self, scan: &mut Scan) {
        for room in 0..self.level.rooms.len() {
            self.scan_slabs(room, scan);
            if scan.r > 0 && !scan.done() {
                self.scan_walls(room, scan);
            }
            if scan.done() {
                break;
            }
        }
    }

    /// Does the cylinder's bounding box touch the room footprint?
    #[inline(always)]
    fn touches(&self, room: usize, scan: &Scan) -> bool {
        let rec = self.rec(room);
        let (x, z, r) = (scan.x, scan.z, scan.r);
        x + r >= rec.origin[0]
            && z + r >= rec.origin[2]
            && x - r < rec.origin[0] + i32::from(rec.width) * self.s
            && z - r < rec.origin[2] + i32::from(rec.depth) * self.s
    }

    /// Floor and ceiling slabs of one room under the five samples.
    ///
    /// Samples that fall in the same flat cell as an earlier one would offer
    /// the identical interval again, so they are skipped.
    #[inline(never)]
    fn scan_slabs(&self, room: usize, scan: &mut Scan) {
        if !self.touches(room, scan) {
            return;
        }
        let Some(view) = self.view(room) else {
            if scan.foreign {
                self.scan_foreign(room, scan);
            }
            return;
        };
        let rec = self.rec(room);
        let mut flat = u32::MAX;
        for i in 0..if scan.r > 0 { 5 } else { 1 } {
            let (sx, sz) = scan.sample(i);
            let Some((cx, cz, lx, lz)) = self.cell_of(
                sx - rec.origin[0],
                sz - rec.origin[2],
                view.width(),
                view.depth(),
            ) else {
                continue;
            };
            let key = cx as u32 | (cz as u32) << 8;
            if key != flat && self.scan_sample(room, view, key | lx << 16 | lz << 24, scan) {
                flat = key;
            }
        }
    }

    /// A non-resident room is solid over its footprint and `min_y..max_y`.
    #[inline(never)]
    fn scan_foreign(&self, room: usize, scan: &mut Scan) {
        let rec = self.rec(room);
        let (w, d) = (usize::from(rec.width), usize::from(rec.depth));
        for i in 0..if scan.r > 0 { 5 } else { 1 } {
            let (sx, sz) = scan.sample(i);
            if self
                .cell_of(sx - rec.origin[0], sz - rec.origin[2], w, d)
                .is_some()
            {
                if rec.max_y > rec.min_y {
                    let src = Src {
                        kind: kind::FOREIGN,
                        room: room as u16,
                        ..Src::default()
                    };
                    let oy = rec.origin[1];
                    scan.offer(
                        oy + i32::from(rec.min_y) - scan.h,
                        oy + i32::from(rec.max_y),
                        src,
                    );
                }
                return;
            }
        }
    }

    /// Floor and ceiling slabs of the sector `cell = cx | cz << 8 | lx << 16
    /// | lz << 24` (sector and in-sector offsets). True when the intervals
    /// are the same anywhere in the sector (flat floor, flat or no solid
    /// ceiling).
    #[inline(never)]
    fn scan_sample(
        &self,
        room: usize,
        view: &CollisionView<'a>,
        cell: u32,
        scan: &mut Scan,
    ) -> bool {
        let (cx, cz) = (cell & 255, (cell >> 8) & 255);
        let (lx, lz) = ((cell >> 16) & 255, cell >> 24);
        let Some(sector) = view.sectors.get(cz as usize * view.width() + cx as usize) else {
            return false;
        };
        let (oy, h) = (self.rec(room).origin[1], scan.h);
        let mut src = Src {
            kind: kind::FLOOR,
            aux: 0,
            cx: cx as u8,
            cz: cz as u8,
            room: room as u16,
        };
        let mut flat = true;
        if let Some((f, second)) = self.sector_floor(view, sector, lx, lz) {
            src.aux = u8::from(second);
            let top = oy + f;
            scan.offer(top - SLAB_THICKNESS - h, top, src);
            flat = sector.flags & SECTOR_SPLIT_LEVEL == 0
                && sector.floor[0] == sector.floor[1]
                && sector.floor[1] == sector.floor[2]
                && sector.floor[2] == sector.floor[3];
        }
        if sector.flags & SECTOR_CEIL_SOLID != 0 {
            if let Some(c) = self.sector_ceil(view, sector, lx, lz) {
                src.kind = kind::CEIL;
                let bottom = oy + c;
                scan.offer(bottom - h, bottom + SLAB_THICKNESS, src);
                flat &= sector.flags & SECTOR_SPLIT_LEVEL == 0
                    && sector.ceil[0] == sector.ceil[1]
                    && sector.ceil[1] == sector.ceil[2]
                    && sector.ceil[2] == sector.ceil[3];
            }
        }
        flat
    }

    /// Set up the cell range the circle overlaps in a resident room. False
    /// when it misses the room.
    #[inline(never)]
    fn wall_begin(&self, room: usize, scan: &mut Scan) -> bool {
        if !self.touches(room, scan) || self.view(room).is_none() {
            return false;
        }
        let s = self.s;
        let rec = self.rec(room);
        let (w, d) = (i32::from(rec.width), i32::from(rec.depth));
        let (lx, lz, r) = (scan.x - rec.origin[0], scan.z - rec.origin[2], scan.r);
        let cell = |v: i32, n: i32| self.recip.cell(v.max(0).min(n * s - 1) as u32) as u8;
        scan.wx0 = cell(lx - r, w);
        scan.wx1 = cell(lx + r, w);
        scan.wcx = scan.wx0;
        scan.wcz = cell(lz - r, d);
        scan.wz1 = cell(lz + r, d);
        true
    }

    /// Walk the cells the circle overlaps.
    #[inline(never)]
    fn scan_walls(&self, room: usize, scan: &mut Scan) {
        if !self.wall_begin(room, scan) {
            return;
        }
        loop {
            self.wall_cell(room, scan);
            scan.wcx += 1;
            if scan.wcx > scan.wx1 {
                scan.wcx = scan.wx0;
                scan.wcz += 1;
                if scan.wcz > scan.wz1 {
                    break;
                }
            }
        }
    }

    /// Wall pieces of the scan's current cell against the circle.
    #[inline(never)]
    fn wall_cell(&self, room: usize, scan: &mut Scan) {
        let Some(view) = self.view(room) else {
            return;
        };
        let Some(ext) = view
            .sectors
            .get(usize::from(scan.wcz) * view.width() + usize::from(scan.wcx))
            .and_then(|sector| view.ext(sector))
        else {
            return;
        };
        let rec = self.rec(room);
        scan.px = scan.x - rec.origin[0] - i32::from(scan.wcx) * self.s;
        scan.pz = scan.z - rec.origin[2] - i32::from(scan.wcz) * self.s;
        for wall in view.sector_walls(ext) {
            self.test_wall(wall, room, scan);
        }
    }

    /// One wall piece against the circle.
    #[inline(never)]
    fn test_wall(&self, wall: &ColWall, room: usize, scan: &mut Scan) {
        if !scan.hits_wall(wall.edge, self.s) {
            return;
        }
        let (lo, hi) = wall_span(wall);
        if hi > lo {
            let src = Src {
                kind: kind::WALL,
                aux: wall.edge,
                cx: scan.wcx,
                cz: scan.wcz,
                room: room as u16,
            };
            let oy = self.rec(room).origin[1];
            scan.offer(oy + lo - scan.h, oy + hi, src);
        }
    }

    /// Is a point (or an upright body of height `h` over a single footprint
    /// sample) inside a slab? The cheap path for `r == 0` queries: it skips
    /// the [`Scan`] accumulator and tests rooms by footprint first.
    #[inline(never)]
    pub(crate) fn point_blocked(&self, p: &[i32; 3], h: i32, foreign: bool) -> bool {
        let (s, t, y) = (self.s, SLAB_THICKNESS, p[1]);
        for (room, rec) in self.level.rooms.iter().enumerate() {
            let (lx, lz) = (p[0] - rec.origin[0], p[2] - rec.origin[2]);
            if lx < 0 || lz < 0 || lx >= i32::from(rec.width) * s || lz >= i32::from(rec.depth) * s
            {
                continue;
            }
            let oy = rec.origin[1];
            let Some(view) = self.view(room) else {
                if foreign
                    && rec.max_y > rec.min_y
                    && y > oy + i32::from(rec.min_y) - h
                    && y < oy + i32::from(rec.max_y)
                {
                    return true;
                }
                continue;
            };
            let Some((cx, cz, ox, oz)) = self.cell_of(lx, lz, view.width(), view.depth()) else {
                continue;
            };
            let Some(sector) = view.sectors.get(cz * view.width() + cx) else {
                continue;
            };
            if let Some((f, _)) = self.sector_floor(view, sector, ox, oz) {
                if y > oy + f - t - h && y < oy + f {
                    return true;
                }
            }
            if sector.flags & SECTOR_CEIL_SOLID != 0 {
                if let Some(c) = self.sector_ceil(view, sector, ox, oz) {
                    if y > oy + c - h && y < oy + c + t {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// True when a cylinder with feet at `p` overlaps solid.
    pub fn body_blocked(&self, p: [i32; 3], radius: i32, height: i32) -> bool {
        let mut scan = Scan::any(p, radius, height, true);
        self.scan_column(&mut scan);
        scan.found
    }

    /// True when the point is strictly inside a floor or ceiling slab of a
    /// resident room.
    pub fn point_in_solid(&self, p: [i32; 3]) -> bool {
        let mut scan = Scan::any(p, 0, 0, false);
        self.scan_column(&mut scan);
        scan.found
    }
}
