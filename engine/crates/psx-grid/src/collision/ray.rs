//! Point rays: exact cell traversal for point traces and line of sight.
//!
//! The ray is walked cell by cell with integer crossing times (Q12 of the
//! segment). Per cell, wall pieces are tested by exact plane crossing and the
//! floor and ceiling slabs by the side (above, below, inside) of the ray at
//! the ends of pieces split where a triangle diagonal is crossed, so each
//! piece is linear. A hit is then validated against the discrete slab
//! predicate, so the returned point is never strictly inside a slab.
//!
//! Frames are kept small on purpose (the motor runs the provider on a 1 KiB
//! stack): state travels in two small structs ([`Query`], [`Cell`]) passed by
//! reference and results come back as scalars or through `&mut Raw`.

use super::body::{back_code, solid_start, Query, Raw};
use super::scan::{kind, nc, Src};
use super::world::{wall_code, GridCollision, COORD_LIMIT, SLAB_THICKNESS};
use crate::format::*;
use crate::height::{in_first_triangle, weight_q12};
use psx_math::int32::mul_div_i32;

/// Sentinel for "no such slab" in [`GridCollision::slab_g`].
const NO_SLAB: i32 = i32::MIN;
/// Slack for interpolated heights, which round by up to two units against the
/// exact plane the corner range bounds.
const SLAB_MARGIN: i32 = 3;

/// Record a hit in the accumulator if it is earlier than the current one.
#[inline(always)]
fn consider(raw: &mut Raw, q: i32, code: u8, src: Src) {
    if q < raw.fraction {
        raw.fraction = q;
        raw.normal = code;
        raw.src = src;
    }
}

/// Walk state of one ray through one room: the room-local ray start, the
/// room size in cells, the cell and piece `[q0, q1]` (Q12 of the segment)
/// being tested, the sub-piece `[s0, e0]` and slab (`which`: 0 floor, 1
/// ceiling) of the slab test, and `entry`, the normal code for a cell entered
/// sideways. Small integer types keep it (and the frame holding it) short.
#[derive(Default)]
pub(crate) struct Cell {
    ax: i32,
    az: i32,
    tx: i32,
    tz: i32,
    ga: i32,
    gb: i32,
    room: u16,
    cx: i16,
    cz: i16,
    w: i16,
    dp: i16,
    qb: i16,
    q0: i16,
    q1: i16,
    s0: i16,
    e0: i16,
    flags: u16,
    which: u8,
    entry: u8,
}

impl Cell {
    #[inline(always)]
    fn room(&self) -> usize {
        usize::from(self.room)
    }
    #[inline(always)]
    fn cx(&self) -> i32 {
        i32::from(self.cx)
    }
    #[inline(always)]
    fn cz(&self) -> i32 {
        i32::from(self.cz)
    }
    #[inline(always)]
    fn q0(&self) -> i32 {
        i32::from(self.q0)
    }
    #[inline(always)]
    fn q1(&self) -> i32 {
        i32::from(self.q1)
    }
    #[inline(always)]
    fn sector_index(&self, width: usize) -> usize {
        self.cz as usize * width + self.cx as usize
    }
}

/// Crossing time out of cell `cell` along one axis.
#[inline(always)]
fn cross_time(a: i32, cell: i32, dv: i32, s: i32) -> i32 {
    if dv > 0 {
        ((cell + 1) * s - a) * 4096 / dv
    } else if dv < 0 {
        (a - cell * s) * 4096 / -dv
    } else {
        i32::MAX
    }
}

impl<'a> GridCollision<'a> {
    /// Point trace with Quake semantics. The earliest analytic hit is built
    /// in `raw.fraction/normal/src`, then validated against the discrete slab
    /// predicate.
    #[inline(never)]
    pub(crate) fn point_trace(&self, q: &Query, raw: &mut Raw) {
        if self.blocked_t(q, 0) {
            let all = self.blocked_t(q, 4096);
            solid_start(raw, all);
            return;
        }
        raw.fraction = 4097;
        for room in 0..self.level.rooms.len() {
            self.ray_walk(room, q, raw);
        }
        let found = raw.fraction <= 4096;
        let t = if found { raw.fraction } else { 4096 };
        if self.blocked_t(q, t) {
            // The analytic hit and the discrete predicate disagree by a
            // rounding sliver (or a foreign room): settle on a clear sample.
            let (mut lo, mut hi) = (0, t);
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if self.blocked_t(q, mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            raw.fraction = lo;
            if !found {
                raw.contact_hi = hi;
            }
        } else if !found {
            raw.fraction = 4096;
        }
    }

    /// True when nothing solid blocks the segment `a -> b`. Non-resident
    /// rooms do not block; endpoints exactly on a surface do not block.
    #[inline(never)]
    pub fn line_of_sight(&self, a: [i32; 3], b: [i32; 3]) -> bool {
        let clamp = |p: [i32; 3]| p.map(|c| c.max(-COORD_LIMIT).min(COORD_LIMIT));
        let (a, b) = (clamp(a), clamp(b));
        if self.point_in_solid(a) {
            return false;
        }
        let q = Query {
            a,
            d: [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            r: 0,
            h: 0,
            body: false,
        };
        let mut raw = Raw::new();
        raw.fraction = 4097;
        for room in 0..self.level.rooms.len() {
            self.ray_walk(room, &q, &mut raw);
        }
        raw.fraction > 4096
    }

    /// Clip the ray to the room footprint and find its first cell. False when
    /// the ray cannot hit this room (or the room is not resident).
    #[inline(never)]
    fn ray_setup(&self, room: usize, q: &Query, hit: &Raw, c: &mut Cell) -> bool {
        let Some(view) = self.view(room) else {
            return false;
        };
        let rec = self.rec(room);
        let s = self.s;
        let (w, dp) = (view.width() as i32, view.depth() as i32);
        c.room = room as u16;
        c.w = w as i16;
        c.dp = dp as i16;
        c.ax = q.a[0].wrapping_sub(rec.origin[0]);
        c.az = q.a[2].wrapping_sub(rec.origin[2]);
        let lim = COORD_LIMIT * 2;
        if !(-lim..=lim).contains(&c.ax) || !(-lim..=lim).contains(&c.az) {
            return false;
        }
        let d = &q.d;
        let mut qa = 0;
        let mut qb = (hit.fraction + 1).min(4096);
        for (av, dv, ext) in [(c.ax, d[0], w * s), (c.az, d[2], dp * s)] {
            if dv == 0 {
                if av < 0 || av >= ext {
                    return false;
                }
            } else {
                let t1 = (-av) * 4096 / dv;
                let t2 = (ext - av) * 4096 / dv;
                qa = qa.max(t1.min(t2) - 1);
                qb = qb.min(t1.max(t2) + 1);
            }
        }
        if qa > qb {
            return false;
        }
        let cell = |v: i32, n: i32| self.recip.cell(v.max(0).min(n * s - 1) as u32) as i16;
        c.cx = cell(c.ax + d[0] * qa / 4096, w);
        c.cz = cell(c.az + d[2] * qa / 4096, dp);
        c.qb = qb as i16;
        c.q0 = qa as i16;
        c.q1 = qa as i16;
        c.entry = back_code(d);
        true
    }

    /// Walk the ray through one room's cells.
    #[inline(never)]
    fn ray_walk(&self, room: usize, q: &Query, hit: &mut Raw) {
        let mut cell = Cell::default();
        if !self.ray_setup(room, q, hit, &mut cell) {
            return;
        }
        let c = &mut cell;
        let (s, d) = (self.s, &q.d);
        c.tx = cross_time(c.ax, c.cx(), d[0], s);
        c.tz = cross_time(c.az, c.cz(), d[2], s);
        loop {
            c.q1 = c.tx.min(c.tz).min(i32::from(c.qb)).max(c.q0()) as i16;
            self.ray_walls(q, c, hit);
            self.ray_slabs(q, c, hit);
            if c.q1 >= c.qb || hit.fraction <= c.q1() {
                break;
            }
            // Only the axis that was crossed needs a new crossing time.
            let (tx, tz) = (c.tx, c.tz);
            if tx <= tz {
                c.cx += d[0].signum() as i16;
                c.entry = if d[0] > 0 { nc::NX } else { nc::PX };
                c.tx = cross_time(c.ax, c.cx(), d[0], s);
            }
            if tz <= tx {
                c.cz += d[2].signum() as i16;
                if tz < tx {
                    c.entry = if d[2] > 0 { nc::NZ } else { nc::PZ };
                }
                c.tz = cross_time(c.az, c.cz(), d[2], s);
            }
            if c.cx < 0 || c.cx >= c.w || c.cz < 0 || c.cz >= c.dp {
                break;
            }
            c.q0 = c.q1;
        }
    }

    /// Wall pieces of the cell against the whole ray.
    #[inline(never)]
    fn ray_walls(&self, q: &Query, c: &Cell, hit: &mut Raw) {
        let Some(view) = self.view(c.room()) else {
            return;
        };
        let Some(ext) = view
            .sectors
            .get(c.sector_index(view.width()))
            .and_then(|sector| view.ext(sector))
        else {
            return;
        };
        for wall in view.sector_walls(ext) {
            let v = self.wall_cross(wall, q, c);
            if v >= 0 {
                consider(hit, v >> 8, (v & 255) as u8, Src::default());
            }
        }
    }

    /// Crossing of the ray with one wall piece: `(q << 8) | normal_code`, or
    /// -1. `q` is the last clear fraction.
    #[inline(never)]
    fn wall_cross(&self, wall: &ColWall, q: &Query, c: &Cell) -> i32 {
        let s = self.s;
        let rec = self.rec(c.room());
        let ax = q.a[0] - rec.origin[0] - c.cx() * s;
        let az = q.a[2] - rec.origin[2] - c.cz() * s;
        let d = &q.d;
        let (nx, nz, k) = match wall.edge {
            edge::NORTH => (0, 1, 0),
            edge::SOUTH => (0, 1, s),
            edge::EAST => (1, 0, s),
            edge::WEST => (1, 0, 0),
            edge::NWSE => (1, -1, 0),
            edge::NESW => (1, 1, s),
            _ => return -1,
        };
        let ds = nx * d[0] + nz * d[2];
        let side0 = nx * ax + nz * az - k;
        let side1 = side0 + ds;
        if !((side0 > 0 && side1 < 0) || (side0 < 0 && side1 > 0)) {
            return -1;
        }
        let (num, den) = (side0.abs(), ds.abs());
        let x = ax + mul_div_i32(d[0], num, den);
        let z = az + mul_div_i32(d[2], num, den);
        let y = q.a[1] - rec.origin[1] + mul_div_i32(d[1], num, den);
        let amount = match wall.edge {
            edge::NORTH | edge::NWSE => x,
            edge::EAST => z,
            edge::SOUTH | edge::NESW => s - x,
            _ => s - z,
        };
        if !(0..=s).contains(&amount) {
            return -1;
        }
        let w = weight_q12(amount as u32, self.inv24);
        let [ba, bb, tb, ta] = wall.y.map(i32::from);
        if y < ba + (((bb - ba) * w) >> 12) || y > ta + (((tb - ta) * w) >> 12) {
            return -1;
        }
        (mul_div_i32(num, 4096, den) << 8) | i32::from(wall_code(wall.edge, s, ax, az))
    }

    /// Cell-local plan offset of the ray at `t`, clamped into the sector.
    #[inline(always)]
    fn cell_local(&self, q: &Query, c: &Cell, t: i32) -> (u32, u32) {
        let rec = self.rec(c.room());
        let s = self.s;
        let p = q.at(t);
        (
            (p[0] - rec.origin[0] - c.cx() * s).max(0).min(s) as u32,
            (p[2] - rec.origin[2] - c.cz() * s).max(0).min(s) as u32,
        )
    }

    /// Signed heights of the ray at `t` against slab `c.which` of the cell:
    /// `(y - top, y - bottom)`. Clear above when the first is `>= 0`, clear
    /// below when the second is `<= 0`, inside otherwise. The first is
    /// [`NO_SLAB`] when the cell has no such slab.
    #[inline(never)]
    fn slab_g(&self, q: &Query, c: &Cell, t: i32) -> (i32, i32) {
        let Some(view) = self.view(c.room()) else {
            return (NO_SLAB, 0);
        };
        let (Some(sector), Some(rec)) = (
            view.sectors.get(c.sector_index(view.width())),
            self.level.rooms.get(c.room()),
        ) else {
            return (NO_SLAB, 0);
        };
        let s = self.s;
        let p = q.at(t);
        let lx = (p[0] - rec.origin[0] - c.cx() * s).max(0).min(s) as u32;
        let lz = (p[2] - rec.origin[2] - c.cz() * s).max(0).min(s) as u32;
        let oy = rec.origin[1];
        let (lo, hi) = if c.which == 0 {
            let Some((f, _)) = self.sector_floor(view, sector, lx, lz) else {
                return (NO_SLAB, 0);
            };
            (oy + f - SLAB_THICKNESS, oy + f)
        } else {
            let (true, Some(h)) = (
                sector.flags & SECTOR_CEIL_SOLID != 0,
                self.sector_ceil(view, sector, lx, lz),
            ) else {
                return (NO_SLAB, 0);
            };
            (oy + h, oy + h + SLAB_THICKNESS)
        };
        (p[1] - hi, p[1] - lo)
    }

    /// Floor and solid-ceiling slabs of the cell against the piece.
    ///
    /// A piece whose heights stay clear of a slab's whole corner range (plus
    /// a margin for interpolation rounding) cannot touch it and is skipped
    /// before any height is evaluated. The remaining slabs are walked over
    /// their linear sub-pieces, evaluating each sub-piece end once.
    #[inline(never)]
    fn ray_slabs(&self, q: &Query, c: &mut Cell, hit: &mut Raw) {
        let Some(view) = self.view(c.room()) else {
            return;
        };
        let Some(sector) = view.sectors.get(c.sector_index(view.width())) else {
            return;
        };
        let flags = sector.flags;
        let oy = self.rec(c.room()).origin[1];
        let (ya, yb) = (q.y_at(c.q0()), q.y_at(c.q1()));
        let (ymin, ymax) = (ya.min(yb), ya.max(yb));
        let mut want = [false; 2];
        if flags & SECTOR_HAS_FLOOR != 0 {
            let (lo, hi) = self.floor_range(view, sector);
            want[0] =
                ymin <= oy + hi + SLAB_MARGIN && ymax >= oy + lo - SLAB_THICKNESS - SLAB_MARGIN;
        }
        if flags & SECTOR_CEIL_SOLID != 0 && flags & SECTOR_HAS_CEIL != 0 {
            let (lo, hi) = self.ceil_range(view, sector);
            want[1] =
                ymax >= oy + lo - SLAB_MARGIN && ymin <= oy + hi + SLAB_THICKNESS + SLAB_MARGIN;
        }
        if !want[0] && !want[1] {
            return;
        }
        // Where the piece crosses each triangle diagonal, if it does.
        let s = self.s;
        let (l0x, l0z) = self.cell_local(q, c, c.q0());
        let (l1x, l1z) = self.cell_local(q, c, c.q1());
        let brk = |nesw: bool| -> i32 {
            let diag = |lx: u32, lz: u32| -> i32 {
                if nesw {
                    (lx + lz) as i32 - s
                } else {
                    lx as i32 - lz as i32
                }
            };
            let (e0, e1) = (diag(l0x, l0z), diag(l1x, l1z));
            if (e0 < 0 && e1 > 0) || (e0 > 0 && e1 < 0) {
                c.q0() + (c.q1() - c.q0()) * e0.abs() / (e0.abs() + e1.abs()).max(1)
            } else {
                c.q1()
            }
        };
        let mut b1 = if want[0] {
            brk(flags & SECTOR_FLOOR_NESW != 0)
        } else {
            c.q1()
        };
        let mut b2 = if want[1] {
            brk(flags & SECTOR_CEIL_NESW != 0)
        } else {
            c.q1()
        };
        if b1 > b2 {
            core::mem::swap(&mut b1, &mut b2);
        }
        c.flags = flags;
        let pts = [c.q0(), b1, b2, c.q1()];
        for which in 0..2 {
            if !want[usize::from(which)] {
                continue;
            }
            c.which = which;
            let (ga, gb) = self.slab_g(q, c, pts[0]);
            if ga == NO_SLAB {
                continue;
            }
            c.ga = ga;
            c.gb = gb;
            for i in 0..3 {
                let (s0, e0) = (pts[i], pts[i + 1]);
                if hit.fraction <= s0 {
                    break;
                }
                if i > 0 && s0 == e0 {
                    continue;
                }
                c.s0 = s0 as i16;
                c.e0 = e0 as i16;
                let v = self.slab_cross(q, c);
                if v >= 0 {
                    self.slab_hit(q, c, v, hit);
                }
            }
        }
    }

    /// Where the ray leaves clear space on the sub-piece `[c.s0, c.e0]` for
    /// slab `c.which`, given its start heights in `c.ga/gb` (updated to the
    /// end heights): the last clear fraction, or -1. Entering sideways
    /// reports one before the piece start with bit 16 set; passing below
    /// sets bit 17.
    #[inline(never)]
    fn slab_cross(&self, q: &Query, c: &mut Cell) -> i32 {
        let (s0, e0) = (i32::from(c.s0), i32::from(c.e0));
        let (a0, b0) = (c.ga, c.gb);
        let (a1, b1) = self.slab_g(q, c, e0);
        c.ga = a1;
        c.gb = b1;
        let side = |a: i32, b: i32| {
            if a >= 0 {
                1
            } else if b <= 0 {
                -1
            } else {
                0
            }
        };
        let side0 = side(a0, b0);
        if side0 == 0 {
            return if s0 > 0 { (s0 - 1) | (1 << 16) } else { -1 };
        }
        if side(a1, b1) == side0 {
            return -1;
        }
        let clear = |t: i32| {
            let (a, b) = self.slab_g(q, c, t);
            side(a, b) == side0
        };
        // Both heights are linear on the piece: interpolate the crossing,
        // then confirm it is the last clear sample (bisect if rounding
        // moved it).
        let (g0, g1) = if side0 > 0 { (a0, a1) } else { (-b0, -b1) };
        let guess = (s0 + (e0 - s0) * g0 / (g0 - g1).max(1)).max(s0).min(e0 - 1);
        let t = if clear(guess) && (guess + 1 >= e0 || !clear(guess + 1)) {
            guess
        } else {
            let (mut lo, mut hi) = (s0, e0);
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if clear(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        t | if side0 < 0 { 2 << 16 } else { 0 }
    }

    /// Record the crossing `v` from [`Self::slab_cross`] with its normal.
    #[inline(never)]
    fn slab_hit(&self, q: &Query, c: &Cell, v: i32, hit: &mut Raw) {
        let t = v & 0xFFFF;
        let mut src = Src::default();
        let code = if v & (1 << 16) != 0 {
            if t + 1 == c.s0 as i32 && c.s0 == c.q0 {
                c.entry
            } else {
                back_code(&q.d)
            }
        } else if v & (2 << 16) != 0 {
            nc::DOWN
        } else if c.which == 0 {
            let (lx, lz) = self.cell_local(q, c, t);
            let nesw = c.flags & SECTOR_FLOOR_NESW != 0;
            src = Src {
                kind: kind::FLOOR,
                aux: u8::from(!in_first_triangle(nesw, lx, lz, self.s as u32)),
                cx: c.cx as u8,
                cz: c.cz as u8,
                room: c.room,
            };
            nc::FLOOR
        } else {
            nc::UP
        };
        consider(hit, t, code, src);
    }
}
