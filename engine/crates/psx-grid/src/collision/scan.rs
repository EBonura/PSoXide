//! Column scans: the forbidden feet-height intervals every body trace is
//! built from.
//!
//! The scan is deliberately non-generic and writes into a small [`Scan`]
//! accumulator owned by the caller, so each function in the chain keeps a
//! frame of a few dozen bytes (the motor runs the provider on a 1 KiB stack).

use super::world::{
    cell_in, circle_hits_wall, wall_applies, wall_span, GridCollision, SLAB_THICKNESS,
};
use crate::format::*;

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
    /// Upward normal of the platform plane named by the hit's [`Src`].
    pub const PLATFORM_TOP: u8 = 12;
    /// Normal of the wall plane named by the hit's [`Src`], facing the side
    /// its `aux` names.
    pub const WALL_PLANE: u8 = 13;
}

/// Source kinds.
pub(crate) mod kind {
    pub const FLOOR: u8 = 1;
    pub const CEIL: u8 = 2;
    pub const WALL: u8 = 3;
    pub const FOREIGN: u8 = 4;
    /// A free plane; `aux` is the plane kind, or 4/5 for the negative/positive
    /// side of a wall plane a ray crossed, and `cx | cz << 8` the index in
    /// the room's `COL_PLANES`.
    pub const PLANE: u8 = 5;
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
    /// Origin height and index of the room being scanned.
    pub oy: i32,
    pub room: u16,
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
            oy: 0,
            room: 0,
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
    pub(crate) fn offer(&mut self, lo: i32, hi: i32, src: Src) {
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

    /// Compass sample `i` of the cylinder footprint: the centre, then
    /// `+x`, `-x`, `+z`, `-z` at radius `r`. Arithmetic rather than a `match`
    /// so no jump table has to be read from RAM.
    #[inline(always)]
    pub(crate) fn sample(&self, i: i32) -> (i32, i32) {
        let r = self.r;
        let dx = if i == 1 {
            r
        } else if i == 2 {
            -r
        } else {
            0
        };
        let dz = if i == 3 {
            r
        } else if i == 4 {
            -r
        } else {
            0
        };
        (self.x + dx, self.z + dz)
    }
}

impl<'a> GridCollision<'a> {
    /// Scan every room for intervals at the scan's column.
    #[inline(always)]
    pub(crate) fn scan_column(&self, scan: &mut Scan) {
        let near = self.visit(scan.x, scan.z, scan.r);
        for step in 0..near.len(self.rooms.len()) {
            let room = near.room(step);
            let walls = self.scan_slabs(room, scan);
            if walls && scan.r > 0 && !scan.done() {
                self.scan_walls(room, scan);
            }
            if self.planes && !scan.done() {
                self.scan_planes(room, scan);
            }
            if scan.done() {
                break;
            }
        }
    }

    /// Floor and ceiling slabs of one room under the five samples. False
    /// when the room cannot contribute walls to this scan.
    ///
    /// * A cylinder no wider than a sector centred on a [`SECTOR_CLEAR`]
    ///   sector sees one floor slab, at most one ceiling slab and no walls.
    /// * One that stays inside a sector with flat slabs sees only that
    ///   sector's slabs, and walls only if the sector has any.
    /// * Otherwise each distinct flat sector under a sample is offered once
    ///   (the other samples in it would repeat the interval) and a sloped
    ///   one is evaluated per sample.
    #[inline(never)]
    fn scan_slabs(&self, room: usize, scan: &mut Scan) -> bool {
        let rec = self.rec(room);
        let (ox, oz) = (rec.origin[0], rec.origin[2]);
        let (vw, vd) = (usize::from(rec.width), usize::from(rec.depth));
        let (x, z, r) = (scan.x, scan.z, scan.r);
        // Constants loaded once: the calls below would force reloads.
        let (recip, s) = (self.recip, self.s);
        let ext = (vw as u32 * s as u32, vd as u32 * s as u32);
        if x + r < ox || z + r < oz || x - r >= ox + ext.0 as i32 || z - r >= oz + ext.1 as i32 {
            return false;
        }
        let Some(view) = self.view(room) else {
            if scan.foreign {
                self.scan_foreign(room, scan);
            }
            return false;
        };
        scan.oy = rec.origin[1];
        scan.room = room as u16;
        let sectors = view.sectors;
        // Sector keys already offered as flat.
        let mut seen = [u32::MAX; 4];
        let mut taken = 0;
        for i in 0..if scan.r > 0 { 5 } else { 1 } {
            let (sx, sz) = scan.sample(i);
            let Some((cx, cz, lx, lz)) = cell_in(&recip, s as u32, (sx - ox, sz - oz), ext) else {
                continue;
            };
            let key = cx as u32 | (cz as u32) << 8;
            if seen.contains(&key) {
                continue;
            }
            let Some(sector) = sectors.get(cz * vw + cx) else {
                continue;
            };
            let flat = Self::slabs_flat(sector);
            if i == 0 && scan.r <= s {
                let r = scan.r as u32;
                let inside = lx >= r && lx + r < s as u32 && lz >= r && lz + r < s as u32;
                if sector.flags & SECTOR_CLEAR != 0 {
                    self.offer_flat(room, key, sector, scan);
                    return false;
                }
                if inside && flat {
                    self.offer_flat(room, key, sector, scan);
                    return sector.ext != NO_EXT;
                }
            }
            if flat {
                self.offer_flat(room, key, sector, scan);
                seen[taken & 3] = key;
                taken += 1;
            } else {
                self.scan_sample(sector, key | lx << 16 | lz << 24, scan);
            }
        }
        true
    }

    /// Flat floor (or none) and flat solid ceiling (or none): the sector's
    /// slab intervals are the same everywhere in it.
    #[inline(always)]
    fn slabs_flat(sector: &Sector) -> bool {
        let f = sector.flags;
        (f & SECTOR_HAS_FLOOR == 0 || f & SECTOR_FLAT_FLOOR != 0)
            && (f & SECTOR_CEIL_SOLID == 0 || f & SECTOR_HAS_CEIL == 0 || f & SECTOR_FLAT_CEIL != 0)
    }

    /// Offer the floor and solid-ceiling slabs of a [`Self::slabs_flat`]
    /// sector (`key = cx | cz << 8`).
    #[inline(never)]
    fn offer_flat(&self, room: usize, key: u32, sector: &Sector, scan: &mut Scan) {
        let (oy, h) = (scan.oy, scan.h);
        let mut src = Src {
            kind: kind::FLOOR,
            aux: 0,
            cx: (key & 255) as u8,
            cz: ((key >> 8) & 255) as u8,
            room: room as u16,
        };
        if sector.flags & SECTOR_HAS_FLOOR != 0 {
            let top = oy + i32::from(sector.floor[0]);
            scan.offer(top - SLAB_THICKNESS - h, top, src);
        }
        if sector.flags & SECTOR_CEIL_SOLID != 0 && sector.flags & SECTOR_HAS_CEIL != 0 {
            src.kind = kind::CEIL;
            let bottom = oy + i32::from(sector.ceil[0]);
            scan.offer(bottom - h, bottom + SLAB_THICKNESS, src);
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

    /// Floor and ceiling slabs of a sloped (or split) sector under one
    /// sample; `cell = cx | cz << 8 | lx << 16 | lz << 24` (sector and
    /// in-sector offsets).
    #[inline(never)]
    fn scan_sample(&self, sector: &Sector, cell: u32, scan: &mut Scan) {
        let (lx, lz) = ((cell >> 16) & 255, cell >> 24);
        let (oy, h) = (scan.oy, scan.h);
        let flags = sector.flags;
        // Only a split-level sector needs the view (its second triangle).
        let view = if flags & SECTOR_SPLIT_LEVEL != 0 {
            self.view(usize::from(scan.room))
        } else {
            None
        };
        let mut src = Src {
            kind: kind::FLOOR,
            aux: 0,
            cx: (cell & 255) as u8,
            cz: ((cell >> 8) & 255) as u8,
            room: scan.room,
        };
        if let Some((f, second)) = self.sector_floor(view, sector, lx, lz) {
            src.aux = u8::from(second);
            let top = oy + f;
            scan.offer(top - SLAB_THICKNESS - h, top, src);
        }
        if flags & SECTOR_CEIL_SOLID != 0 {
            if let Some(c) = self.sector_ceil(view, sector, lx, lz) {
                src.kind = kind::CEIL;
                let bottom = oy + c;
                scan.offer(bottom - h, bottom + SLAB_THICKNESS, src);
            }
        }
    }

    /// Wall pieces closer than `r` to the circle centre, over the cells the
    /// circle overlaps. Only called after [`Self::scan_slabs`] accepted the
    /// room (so its footprint is touched and `scan.oy` is set).
    #[inline(never)]
    fn scan_walls(&self, room: usize, scan: &mut Scan) {
        let Some(view) = self.view(room) else {
            return;
        };
        let rec = self.rec(room);
        let s = self.s;
        let r = scan.r;
        let (w, d) = (i32::from(rec.width), i32::from(rec.depth));
        let (lx, lz) = (scan.x - rec.origin[0], scan.z - rec.origin[2]);
        let recip = self.recip;
        let cell = |v: i32, n: i32| recip.cell(v.max(0).min(n * s - 1) as u32) as usize;
        let (cx0, cx1) = (cell(lx - r, w), cell(lx + r, w));
        let (cz0, cz1) = (cell(lz - r, d), cell(lz + r, d));
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                let Some(sector) = view.sectors.get(cz * w as usize + cx) else {
                    continue;
                };
                if sector.ext == NO_EXT {
                    continue;
                }
                let Some(ext) = view.ext(sector) else {
                    continue;
                };
                let (px, pz) = (lx - cx as i32 * s, lz - cz as i32 * s);
                for wall in view.sector_walls(ext) {
                    if !circle_hits_wall(wall.edge, s, px, pz, r) || !wall_applies(wall, s, px, pz)
                    {
                        continue;
                    }
                    let (lo, hi) = wall_span(wall);
                    if hi > lo {
                        let src = Src {
                            kind: kind::WALL,
                            aux: wall.edge,
                            cx: cx as u8,
                            cz: cz as u8,
                            room: room as u16,
                        };
                        scan.offer(scan.oy + lo - scan.h, scan.oy + hi, src);
                    }
                }
            }
        }
    }

    /// Is a point (or an upright body of height `h` over a single footprint
    /// sample) inside a slab? The cheap path for `r == 0` queries: it skips
    /// the [`Scan`] accumulator and tests rooms by footprint first.
    #[inline(never)]
    pub(crate) fn point_blocked(&self, p: &[i32; 3], h: i32, foreign: bool) -> bool {
        let (s, t, y) = (self.s, SLAB_THICKNESS, p[1]);
        let near = self.visit(p[0], p[2], 0);
        for step in 0..near.len(self.rooms.len()) {
            let room = near.room(step);
            let rec = self.rec(room);
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
            if let Some((f, _)) = self.sector_floor(Some(view), sector, ox, oz) {
                if y > oy + f - t - h && y < oy + f {
                    return true;
                }
            }
            if sector.flags & SECTOR_CEIL_SOLID != 0 {
                if let Some(c) = self.sector_ceil(Some(view), sector, ox, oz) {
                    if y > oy + c - h && y < oy + c + t {
                        return true;
                    }
                }
            }
            if self.planes
                && Self::plane_blocks_point(view, sector, oy, (lx, lz), y, h, self.camera_pass)
            {
                return true;
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
