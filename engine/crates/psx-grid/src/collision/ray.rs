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
use super::leaf::LeafAt;
use super::scan::{kind, nc, Src};
use super::world::{wall_code, GridCollision, COORD_LIMIT, SLAB_THICKNESS};
use crate::format::*;
use crate::height::{in_first_triangle, weight_q12};
use crate::view::CollisionView;
use core::mem::MaybeUninit;
use psx_math::int32::mul_div_i32;

/// Sentinel for "no such slab" in [`GridCollision::slab_g`].
const NO_SLAB: i32 = i32::MIN;
/// Slack for interpolated heights, which round by up to two units against the
/// exact plane the corner range bounds.
const SLAB_MARGIN: i32 = 3;
/// `slab_cross` result: the ray starts strictly inside the slab.
const START_IN: i32 = 1 << 18;

/// Record a hit in the accumulator if it is earlier than the current one.
#[inline(always)]
pub(super) fn consider(raw: &mut Raw, q: i32, code: u8, src: Src) {
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
pub(crate) struct Cell<'v> {
    /// The room's sector table, copied out of the (RAM resident) view once so
    /// the per-cell code reads only the sector itself from RAM.
    sectors: &'v [Sector],
    /// The leaf record under the ray in a tree room (`tree`), set per leaf
    /// before its walls and slabs are tested; while an extra span of a column
    /// is tested (`span != 0`), that span, in either layout.
    rec: Option<&'v Sector>,
    /// Frame of that leaf: origin and reciprocal. A dense room derives its
    /// frame from the cell.
    ox: i32,
    oz: i32,
    inv: u32,
    size: i16,
    /// Quarter sector code of the leaf, see [`LeafAt::quarter`].
    quarter: u8,
    tree: bool,
    oy: i32,
    ax: i32,
    az: i32,
    ga: i32,
    gb: i32,
    room: u16,
    cx: i16,
    cz: i16,
    w: i16,
    dp: i16,
    qb: i16,
    /// Cell holding the segment end when the whole segment is inside the
    /// room (so a start cell on the same row or column never crosses it),
    /// else -1.
    ex: i16,
    ez: i16,
    /// First sample past this cell along X and Z (`i32::MAX`: never).
    tx: i32,
    tz: i32,
    /// Lowest and highest ray height over the whole segment, room-local.
    ylo: i32,
    yhi: i32,
    q0: i16,
    q1: i16,
    s0: i16,
    e0: i16,
    flags: u16,
    which: u8,
    entry: u8,
    /// 0 while the ground span of the column is tested, else the 1-based
    /// position of the extra span being tested ([`Src::span`]).
    span: u8,
    /// The room has extra spans somewhere ([`CollisionView::span_index`] is
    /// not empty), so the cells' columns are looked up.
    multi: bool,
    /// Index in `SECTORS` of the leaf under the ray in a tree room (the key
    /// of its extra spans); a dense room derives it from the cell.
    id: u16,
}

impl<'v> Cell<'v> {
    /// The sector (or, in a tree room, the leaf) the ray is in; the extra
    /// span being tested, when one is.
    #[inline(always)]
    fn sector(&self) -> Option<&Sector> {
        if self.tree || self.span != 0 {
            return self.rec;
        }
        self.sectors
            .get(self.cz as usize * self.w as usize + self.cx as usize)
    }

    /// Origin, edge and weight reciprocal of the record in `sector()`, given
    /// the level's sector edge and reciprocal.
    #[inline(always)]
    fn frame(&self, s: i32, inv24: u32) -> (i32, i32, i32, u32) {
        if self.tree {
            (self.ox, self.oz, i32::from(self.size), self.inv)
        } else {
            (self.cx() * s, self.cz() * s, s, inv24)
        }
    }
    /// Index of the cell's or leaf's record in `SECTORS`.
    #[inline(always)]
    fn record(&self) -> usize {
        if self.tree {
            usize::from(self.id)
        } else {
            self.cz as usize * self.w as usize + self.cx as usize
        }
    }
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
}

/// First sample `q` of the segment that lies in the cell after `cell` along
/// one axis, for samples `a + trunc(dv * q / 4096)` (truncation toward zero,
/// i.e. toward the start, exactly as [`Query::at`] places them), or
/// `i32::MAX` when the axis does not move.
#[inline(always)]
fn cross_time(a: i32, cell: i32, dv: i32, s: i32) -> i32 {
    if dv > 0 {
        (((cell + 1) * s - a) * 4096 + dv - 1) / dv
    } else if dv < 0 {
        ((a - cell * s + 1) * 4096 - dv - 1) / -dv
    } else {
        i32::MAX
    }
}

/// Samples `q` in `lo..=hi` (empty when `lo > hi`) whose coordinate along one
/// axis is inside `0..ext`, for samples placed like [`cross_time`] does.
#[inline(always)]
fn axis_range(a: i32, dv: i32, ext: i32) -> (i32, i32) {
    if dv > 0 {
        let lo = if a >= 0 { 0 } else { (-a * 4096 + dv - 1) / dv };
        (lo, ((ext - a) * 4096 + dv - 1) / dv - 1)
    } else if dv < 0 {
        let hi = ((a + 1) * 4096 - dv - 1) / -dv - 1;
        let m = a - ext + 1;
        (if m <= 0 { 0 } else { (m * 4096 - dv - 1) / -dv }, hi)
    } else if (0..ext).contains(&a) {
        (0, 4096)
    } else {
        (1, 0)
    }
}

impl<'a> GridCollision<'a> {
    /// Point trace with Quake semantics. The earliest analytic hit is built
    /// in `raw.fraction/normal/src`, then validated against the discrete slab
    /// predicate.
    #[inline(always)]
    pub(crate) fn point_trace(&self, q: &Query<'_>, raw: &mut Raw) {
        // Non-resident rooms are not walked by the ray, so a level that has
        // some needs the explicit start test.
        if self.foreign && self.blocked_t(q, 0) {
            let all = self.blocked_t(q, 4096);
            solid_start(raw, all);
            return;
        }
        raw.fraction = 4097;
        for room in self.rooms_near(q.plan_lo(), q.plan_hi()) {
            self.ray_walk(room, q, raw);
            if self.planes {
                self.ray_planes(room, q, raw);
            }
        }
        if raw.start_solid {
            // The walk found the start inside a slab.
            let all = self.blocked_t(q, 4096);
            *raw = Raw::new();
            solid_start(raw, all);
            return;
        }
        let found = raw.fraction <= 4096;
        let t = if found { raw.fraction } else { 4096 };
        if (self.foreign || self.validate || self.planes) && self.blocked_t(q, t) {
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
                raw.contact_hi = hi as i16;
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
            memo: None,
        };
        let mut raw = Raw::new();
        raw.fraction = 4097;
        for room in self.rooms_near(q.plan_lo(), q.plan_hi()) {
            self.ray_walk(room, &q, &mut raw);
            if self.planes {
                self.ray_planes(room, &q, &mut raw);
            }
        }
        raw.fraction > 4096 && !raw.start_solid
    }

    /// Clip the ray to the room footprint and find its first cell. `None`
    /// when the ray cannot hit this room (or the room is not resident).
    #[inline(never)]
    fn ray_setup<'v>(
        &'v self,
        room: usize,
        q: &Query<'_>,
        hit: &Raw,
        c: &mut MaybeUninit<Cell<'v>>,
    ) -> bool {
        let Some(view) = self.view(room) else {
            return false;
        };
        let rec = self.rec(room);
        let s = self.s;
        let (w, dp) = (view.width() as i32, view.depth() as i32);
        let ax = q.a[0].wrapping_sub(rec.origin[0]);
        let az = q.a[2].wrapping_sub(rec.origin[2]);
        let lim = COORD_LIMIT * 2;
        if !(-lim..=lim).contains(&ax) || !(-lim..=lim).contains(&az) {
            return false;
        }
        let d = &q.d;
        let (ext_x, ext_z) = (w * s, dp * s);
        let (bx, bz) = (ax + d[0], az + d[2]);
        let within = |v: i32, ext: i32| v >= 0 && v < ext;
        // A segment with both ends inside the room needs no clipping.
        let whole =
            within(ax, ext_x) && within(az, ext_z) && within(bx, ext_x) && within(bz, ext_z);
        let (qa, qb) = if whole {
            (0, (hit.fraction + 1).min(4096))
        } else {
            let (x0, x1) = axis_range(ax, d[0], ext_x);
            let (z0, z1) = axis_range(az, d[2], ext_z);
            (
                x0.max(z0).max(0),
                x1.min(z1).min(hit.fraction + 1).min(4096),
            )
        };
        if qa > qb {
            return false;
        }
        let cell = |v: i32, n: i32| self.recip.cell(v.max(0).min(n * s - 1) as u32) as i16;
        c.write(Cell {
            sectors: view.sectors,
            rec: None,
            ox: 0,
            oz: 0,
            inv: 0,
            size: 0,
            quarter: 0,
            tree: view.is_tree(),
            oy: rec.origin[1],
            ax,
            az,
            ga: 0,
            gb: 0,
            room: room as u16,
            cx: cell(ax + d[0] * qa / 4096, w),
            cz: cell(az + d[2] * qa / 4096, dp),
            w: w as i16,
            dp: dp as i16,
            qb: qb as i16,
            ex: if whole { cell(bx, w) } else { -1 },
            ez: if whole { cell(bz, dp) } else { -1 },
            tx: 0,
            tz: 0,
            ylo: q.a[1].min(q.a[1] + d[1]) - rec.origin[1],
            yhi: q.a[1].max(q.a[1] + d[1]) - rec.origin[1],
            q0: qa as i16,
            q1: qa as i16,
            s0: 0,
            e0: 0,
            flags: 0,
            which: 0,
            entry: back_code(d),
            span: 0,
            multi: !view.span_index.is_empty(),
            id: 0,
        });
        true
    }

    /// Walk the ray through one room's cells.
    #[inline(never)]
    fn ray_walk(&self, room: usize, q: &Query<'_>, hit: &mut Raw) {
        let mut cell = MaybeUninit::uninit();
        if !self.ray_setup(room, q, hit, &mut cell) {
            return;
        }
        // SAFETY: `ray_setup` wrote the whole cell when it returned true.
        let c = unsafe { cell.assume_init_mut() };
        let (d, s) = (&q.d, self.s);
        c.tx = if c.cx == c.ex {
            i32::MAX
        } else {
            cross_time(c.ax, c.cx(), d[0], s)
        };
        c.tz = if c.cz == c.ez {
            i32::MAX
        } else {
            cross_time(c.az, c.cz(), d[2], s)
        };
        if c.tree {
            // A tree room is walked sector by sector with no skip routine:
            // each sector resolves its leaf, or its leaves.
            loop {
                let leave = c.tx.min(c.tz);
                c.q1 = (leave - 1).min(i32::from(c.qb)).max(c.q0()) as i16;
                self.ray_tree_cell(q, c, hit);
                if leave > i32::from(c.qb) || hit.fraction <= c.q1() {
                    break;
                }
                if !self.ray_advance(q, c) {
                    break;
                }
            }
            return;
        }
        // Cells with nothing to test are skipped by a leaf routine that
        // keeps its state in registers; it stops at the first cell that needs
        // a call.
        while self.ray_skip(q, c, hit.fraction) {
            if let Some(sector) = c.sector() {
                if sector.ext != NO_EXT {
                    self.ray_walls(q, c, hit);
                }
                if c.multi || self.slabs_matter(q, c, sector) {
                    self.ray_slabs(q, c, hit);
                }
            }
            if c.tx.min(c.tz) > i32::from(c.qb) || hit.fraction <= c.q1() {
                break;
            }
            if !self.ray_advance(q, c) {
                break;
            }
        }
    }

    /// The walls and slabs of one sector of a tree room: of its leaf, or, for
    /// a divided sector, of each of its leaves against the part of the ray's
    /// piece that lies inside that leaf.
    #[inline(never)]
    fn ray_tree_cell<'v>(&'v self, q: &Query<'_>, c: &mut Cell<'v>, hit: &mut Raw) {
        let Some(view) = self.view(c.room()) else {
            return;
        };
        let (cx, cz) = (c.cx as usize, c.cz as usize);
        let slots = Self::cell_slots(view, cx, cz);
        if slots == 1 {
            if let Some(leaf) = self.cell_leaf(view, cx, cz, 0) {
                self.ray_leaf(q, c, hit, &leaf);
            }
            return;
        }
        let (q0, q1, entry) = (c.q0, c.q1, c.entry);
        for k in 0..slots {
            let Some(leaf) = self.cell_leaf(view, cx, cz, k) else {
                continue;
            };
            let (x0, x1) = axis_range(c.ax - leaf.ox, q.d[0], leaf.size);
            let (z0, z1) = axis_range(c.az - leaf.oz, q.d[2], leaf.size);
            let lo = x0.max(z0).max(i32::from(q0));
            let hi = x1.min(z1).min(i32::from(q1));
            if lo > hi {
                continue;
            }
            c.q0 = lo as i16;
            c.q1 = hi as i16;
            // How the ray came into this leaf: from the previous cell, or
            // across one of the leaf's own sides.
            c.entry = if lo <= i32::from(q0) {
                entry
            } else if x0 >= z0 {
                if q.d[0] > 0 {
                    nc::NX
                } else {
                    nc::PX
                }
            } else if q.d[2] > 0 {
                nc::NZ
            } else {
                nc::PZ
            };
            self.ray_leaf(q, c, hit, &leaf);
        }
        c.q0 = q0;
        c.q1 = q1;
        c.entry = entry;
    }

    /// Test one leaf (the cell's frame is set from it) against the piece
    /// `[c.q0, c.q1]`.
    #[inline(always)]
    fn ray_leaf<'v>(&self, q: &Query<'_>, c: &mut Cell<'v>, hit: &mut Raw, leaf: &LeafAt<'v>)
    where
        'a: 'v,
    {
        c.rec = Some(leaf.rec);
        c.ox = leaf.ox;
        c.oz = leaf.oz;
        c.size = leaf.size as i16;
        c.inv = self.dims_of(leaf.rec.flags).1;
        c.quarter = leaf.quarter;
        c.id = leaf.id;
        let sector = leaf.rec;
        if sector.ext != NO_EXT {
            self.ray_walls(q, c, hit);
        }
        if c.multi || self.slabs_matter(q, c, sector) {
            self.ray_slabs(q, c, hit);
        }
    }

    /// Make the first extra span of the cell's (or leaf's) column at or after
    /// position `*next` whose slabs could touch the piece the cell's current
    /// record (see [`Cell::rec`]); false when there is none. `*next` is left
    /// after it.
    #[inline(never)]
    fn next_extra<'v>(&self, q: &Query<'_>, c: &mut Cell<'v>, next: &mut usize) -> bool
    where
        'a: 'v,
    {
        let Some(view) = self.view(c.room()) else {
            return false;
        };
        let extras = view.extra_spans(c.record());
        while let Some(span) = extras.get(*next) {
            *next += 1;
            if self.slabs_matter(q, c, span) {
                c.rec = Some(span);
                c.span = *next as u8;
                return true;
            }
        }
        false
    }

    /// Step to the next cell along the ray, refreshing the crossing times of
    /// the axes crossed. False when the ray leaves the room.
    #[inline(never)]
    fn ray_advance(&self, q: &Query<'_>, c: &mut Cell<'_>) -> bool {
        let (d, s) = (&q.d, self.s);
        let (otx, otz) = (c.tx, c.tz);
        let leave = otx.min(otz);
        if otx <= otz {
            c.cx += d[0].signum() as i16;
            c.entry = if d[0] > 0 { nc::NX } else { nc::PX };
            c.tx = if c.cx == c.ex {
                i32::MAX
            } else {
                cross_time(c.ax, c.cx(), d[0], s)
            };
        }
        if otz <= otx {
            c.cz += d[2].signum() as i16;
            if otz < otx {
                c.entry = if d[2] > 0 { nc::NZ } else { nc::PZ };
            }
            c.tz = if c.cz == c.ez {
                i32::MAX
            } else {
                cross_time(c.az, c.cz(), d[2], s)
            };
        }
        c.q0 = leave as i16;
        !(c.cx < 0 || c.cx >= c.w || c.cz < 0 || c.cz >= c.dp)
    }

    /// Walk through cells that need no wall or slab test, stopping at the
    /// first that does (`true`, its state stored in `c` with `q0 ..= q1`
    /// set) or when the walk is over (`false`). A leaf: the state stays in
    /// registers, which is what makes the common cell cheap.
    #[inline(never)]
    fn ray_skip(&self, q: &Query<'_>, c: &mut Cell<'_>, hit_fraction: i32) -> bool {
        let (d, s) = (&q.d, self.s);
        let (ax, az) = (c.ax, c.az);
        let (ex, ez) = (i32::from(c.ex), i32::from(c.ez));
        let (w, dp, qb) = (i32::from(c.w), i32::from(c.dp), i32::from(c.qb));
        let (ylo, yhi) = (c.ylo, c.yhi);
        let sectors = c.sectors;
        // Rooms without extra spans never look one up.
        let spans: &[u16] = if c.multi {
            self.view(c.room()).map_or(&[], |view| view.span_index)
        } else {
            &[]
        };
        let (mut cx, mut cz, mut q0) = (c.cx(), c.cz(), c.q0());
        let (mut tx, mut tz) = (c.tx, c.tz);
        let mut entry = c.entry;
        loop {
            // This cell holds samples `q0 ..= q1`; the next one starts at
            // `leave`.
            let leave = tx.min(tz);
            let q1 = (leave - 1).min(qb).max(q0);
            if let Some(sector) = sectors.get((cz * w + cx) as usize) {
                let f = sector.flags;
                // A column with extra spans is never skipped, whatever its
                // ground record's flags say.
                let special = if sector.ext != NO_EXT
                    || spans
                        .get((cz * w + cx) as usize)
                        .is_some_and(|&run| run != NO_SPAN)
                {
                    true
                } else if f & (SECTOR_HAS_FLOOR | SECTOR_CEIL_SOLID) == 0 {
                    false
                } else if f & (SECTOR_SPLIT_LEVEL | SECTOR_CEIL_SOLID) == 0
                    && f & SECTOR_FLAT_FLOOR != 0
                {
                    // A flat floor with no solid ceiling: two compares
                    // against the whole segment's height range decide it.
                    let top = i32::from(sector.floor[0]);
                    yhi >= top - SLAB_THICKNESS - SLAB_MARGIN && ylo <= top + SLAB_MARGIN
                } else {
                    true
                };
                if special {
                    c.cx = cx as i16;
                    c.cz = cz as i16;
                    c.q0 = q0 as i16;
                    c.q1 = q1 as i16;
                    c.tx = tx;
                    c.tz = tz;
                    c.entry = entry;
                    return true;
                }
            }
            if leave > qb || hit_fraction <= q1 {
                return false;
            }
            // Only the axes that were crossed need a new crossing time.
            let (otx, otz) = (tx, tz);
            if otx <= otz {
                cx += d[0].signum();
                entry = if d[0] > 0 { nc::NX } else { nc::PX };
                tx = if cx == ex {
                    i32::MAX
                } else {
                    cross_time(ax, cx, d[0], s)
                };
            }
            if otz <= otx {
                cz += d[2].signum();
                if otz < otx {
                    entry = if d[2] > 0 { nc::NZ } else { nc::PZ };
                }
                tz = if cz == ez {
                    i32::MAX
                } else {
                    cross_time(az, cz, d[2], s)
                };
            }
            if cx < 0 || cx >= w || cz < 0 || cz >= dp {
                return false;
            }
            q0 = leave;
        }
    }

    /// Wall pieces of the cell against the whole ray.
    #[inline(never)]
    fn ray_walls(&self, q: &Query<'_>, c: &Cell<'_>, hit: &mut Raw) {
        let Some(sector) = c.sector() else {
            return;
        };
        if sector.ext == NO_EXT {
            return;
        }
        let Some(view) = self.view(c.room()) else {
            return;
        };
        let Some(ext) = view.ext(sector) else {
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
    fn wall_cross(&self, wall: &ColWall, q: &Query<'_>, c: &Cell<'_>) -> i32 {
        let (ox, oz, s, inv) = c.frame(self.s, self.inv24);
        let ax = c.ax - ox;
        let az = c.az - oz;
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
        // One-sided walls (a cliff face) only stop a ray that starts on
        // their solid side: the front is the positive side of north, west
        // and the NW-SE diagonal, the negative side of the others.
        let sided = wall.flags & (WALL_FRONT_ONLY | WALL_BACK_ONLY);
        if sided != 0 {
            let front_positive =
                wall.edge == edge::NORTH || wall.edge == edge::WEST || wall.edge == edge::NWSE;
            let from_front = (side0 > 0) == front_positive;
            if from_front != (sided & WALL_FRONT_ONLY != 0) {
                return -1;
            }
        }
        let (num, den) = (side0.abs(), ds.abs());
        let x = ax + mul_div_i32(d[0], num, den);
        let z = az + mul_div_i32(d[2], num, den);
        let y = q.a[1] - c.oy + mul_div_i32(d[1], num, den);
        let amount = match wall.edge {
            edge::NORTH | edge::NWSE => x,
            edge::EAST => z,
            edge::SOUTH | edge::NESW => s - x,
            _ => s - z,
        };
        // The part of the edge the wall covers, along its walking direction.
        let (from, to) = match wall.flags & (WALL_HALF_FIRST | WALL_HALF_SECOND) {
            WALL_HALF_FIRST => (0, s / 2),
            WALL_HALF_SECOND => (s / 2, s),
            _ => (0, s),
        };
        if !(from..=to).contains(&amount) {
            return -1;
        }
        let w = if to - from == s {
            weight_q12(amount as u32, inv)
        } else {
            weight_q12(((amount - from) * 2) as u32, inv)
        };
        let [ba, bb, tb, ta] = wall.y.map(i32::from);
        if y < ba + (((bb - ba) * w) >> 12) || y > ta + (((tb - ta) * w) >> 12) {
            return -1;
        }
        (mul_div_i32(num, 4096, den) << 8) | i32::from(wall_code(wall.edge, s, ax, az))
    }

    /// The view, only when the sector needs it (split level).
    #[inline(always)]
    fn split_view(&self, c: &Cell<'_>, sector: &Sector) -> Option<&CollisionView<'a>> {
        if sector.flags & SECTOR_SPLIT_LEVEL != 0 {
            self.view(c.room())
        } else {
            None
        }
    }

    /// Cell-local plan offset of the ray at `t`, clamped into the sector.
    #[inline(always)]
    fn cell_local(&self, q: &Query<'_>, c: &Cell<'_>, t: i32) -> (u32, u32) {
        let (ox, oz, s, _) = c.frame(self.s, self.inv24);
        (
            (c.ax + q.d[0] * t / 4096 - ox).max(0).min(s) as u32,
            (c.az + q.d[2] * t / 4096 - oz).max(0).min(s) as u32,
        )
    }

    /// Signed heights of the ray at `t` against slab `c.which` of the cell:
    /// `(y - top, y - bottom)`. Clear above when the first is `>= 0`, clear
    /// below when the second is `<= 0`, inside otherwise. The first is
    /// [`NO_SLAB`] when the cell has no such slab.
    #[inline(never)]
    fn slab_g(&self, q: &Query<'_>, c: &Cell<'_>, t: i32) -> (i32, i32) {
        let Some(sector) = c.sector() else {
            return (NO_SLAB, 0);
        };
        let (lx, lz) = self.cell_local(q, c, t);
        let oy = c.oy;
        // Only a split-level sector needs the view (for its second triangle).
        let view = self.split_view(c, sector);
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
        let y = q.y_at(t);
        (y - hi, y - lo)
    }

    /// Could the piece `[q0, q1]` touch this sector's slabs at all? False
    /// (the common case) when the segment's heights stay clear of both slabs'
    /// whole corner ranges.
    #[inline(never)]
    fn slabs_matter(&self, q: &Query<'_>, c: &Cell<'_>, sector: &Sector) -> bool {
        let flags = sector.flags;
        let solid_ceil = flags & SECTOR_CEIL_SOLID != 0 && flags & SECTOR_HAS_CEIL != 0;
        if flags & SECTOR_HAS_FLOOR == 0 && !solid_ceil {
            return false;
        }
        let (ya, yb) = (q.y_at(c.q0()), q.y_at(c.q1()));
        let (ymin, ymax) = (ya.min(yb), ya.max(yb));
        if flags & SECTOR_HAS_FLOOR != 0 {
            let (lo, hi) = self.floor_range(self.split_view(c, sector), sector);
            if ymin <= c.oy + hi + SLAB_MARGIN && ymax >= c.oy + lo - SLAB_THICKNESS - SLAB_MARGIN {
                return true;
            }
        }
        if solid_ceil {
            let (lo, hi) = self.ceil_range(self.split_view(c, sector), sector);
            if ymax >= c.oy + lo - SLAB_MARGIN && ymin <= c.oy + hi + SLAB_THICKNESS + SLAB_MARGIN {
                return true;
            }
        }
        false
    }

    /// Where the piece crosses the floor and ceiling triangle diagonals (the
    /// piece end when it does not), sorted: `(first, second)`.
    #[inline(never)]
    fn slab_breaks(&self, q: &Query<'_>, c: &Cell<'_>, flags: u16, want: [bool; 2]) -> (i32, i32) {
        let s = c.frame(self.s, self.inv24).2;
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
        let b1 = if want[0] {
            brk(flags & SECTOR_FLOOR_NESW != 0)
        } else {
            c.q1()
        };
        let b2 = if want[1] {
            brk(flags & SECTOR_CEIL_NESW != 0)
        } else {
            c.q1()
        };
        (b1.min(b2), b1.max(b2))
    }

    /// Floor and solid-ceiling slabs of the cell against the piece.
    ///
    /// A piece whose heights stay clear of a slab's whole corner range (plus
    /// a margin for interpolation rounding) cannot touch it and is skipped
    /// before any height is evaluated. The remaining slabs are walked over
    /// their linear sub-pieces, evaluating each sub-piece end once.
    ///
    /// A column of several spans does this for each of its spans in turn,
    /// the ground span first: the extra spans of the cell's record are
    /// made the cell's current record one at a time (`Cell::rec`,
    /// `Cell::span`), and the ground record is put back at the end. The
    /// loop is in this function, not around it, so the slab test is not a
    /// call deeper on the 1 KiB stack.
    #[inline(never)]
    fn ray_slabs<'v>(&self, q: &Query<'_>, c: &mut Cell<'v>, hit: &mut Raw)
    where
        'a: 'v,
    {
        let ground = c.rec;
        let mut next = 0;
        loop {
            self.ray_span_slabs(q, c, hit);
            if !c.multi || !self.next_extra(q, c, &mut next) {
                break;
            }
        }
        if next != 0 {
            c.rec = ground;
            c.span = 0;
        }
    }

    /// The slabs of the cell's current record, see [`Self::ray_slabs`].
    #[inline(always)]
    fn ray_span_slabs(&self, q: &Query<'_>, c: &mut Cell<'_>, hit: &mut Raw) {
        let Some(sector) = c.sector() else {
            return;
        };
        let flags = sector.flags;
        let oy = c.oy;
        let (ya, yb) = (q.y_at(c.q0()), q.y_at(c.q1()));
        let (ymin, ymax) = (ya.min(yb), ya.max(yb));
        let mut want = [false; 2];
        if flags & SECTOR_HAS_FLOOR != 0 {
            let (lo, hi) = self.floor_range(self.split_view(c, sector), sector);
            want[0] =
                ymin <= oy + hi + SLAB_MARGIN && ymax >= oy + lo - SLAB_THICKNESS - SLAB_MARGIN;
        }
        if flags & SECTOR_CEIL_SOLID != 0 && flags & SECTOR_HAS_CEIL != 0 {
            let (lo, hi) = self.ceil_range(self.split_view(c, sector), sector);
            want[1] =
                ymax >= oy + lo - SLAB_MARGIN && ymin <= oy + hi + SLAB_THICKNESS + SLAB_MARGIN;
        }
        if !want[0] && !want[1] {
            return;
        }
        let (b1, b2) = self.slab_breaks(q, c, flags, want);
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
                if s0 > 0 && hit.fraction <= s0 {
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
    fn slab_cross(&self, q: &Query<'_>, c: &mut Cell<'_>) -> i32 {
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
            return if s0 > 0 {
                (s0 - 1) | (1 << 16)
            } else {
                START_IN
            };
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
    fn slab_hit(&self, q: &Query<'_>, c: &Cell<'_>, v: i32, hit: &mut Raw) {
        if v == START_IN {
            hit.start_solid = true;
            return;
        }
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
            let size = c.frame(self.s, self.inv24).2;
            src = Src {
                kind: kind::FLOOR,
                aux: u8::from(!in_first_triangle(nesw, lx, lz, size as u32)) | c.quarter << 3,
                cx: c.cx as u8,
                cz: c.cz as u8,
                room: c.room,
                span: c.span,
            };
            nc::FLOOR
        } else {
            nc::UP
        };
        consider(hit, t, code, src);
    }
}
