//! Point rays: exact cell traversal for point traces and line of sight.
//!
//! The ray is walked cell by cell with integer crossing times (Q12 of the
//! segment). Per cell, wall pieces are tested by exact plane crossing and the
//! floor and ceiling slabs by the sign of `y - surface` at the piece ends,
//! split where a triangle diagonal is crossed so each piece is linear. A hit
//! is then validated against the discrete slab predicate, so the returned
//! point is never strictly inside a slab.

use super::body::{clear, dominant_back_normal, hit, lerp, solid_start};
use super::world::{wall_normal_toward, GridCollision, COORD_LIMIT, SLAB_THICKNESS};
use crate::format::*;
use crate::height::{in_first_triangle, weight_q12};
use crate::view::CollisionView;
use psx_engine::CollisionTrace;
use psx_math::int32::mul_div_i32;

#[derive(Copy, Clone)]
pub(crate) struct RayHit {
    q: i32,
    normal: [i16; 3],
}

fn consider(best: &mut Option<RayHit>, q: i32, normal: [i16; 3]) {
    if best.is_none_or(|b| q < b.q) {
        *best = Some(RayHit { q, normal });
    }
}

/// Largest `q` in `lo..hi` with `ok(q)`, given `ok(lo)` and `!ok(hi)`.
fn last_true(mut lo: i32, mut hi: i32, ok: impl Fn(i32) -> bool) -> i32 {
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if ok(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Horizontal back normal for a cell entered through `entry`.
fn entry_normal(entry: Option<(usize, i32)>, d: [i32; 3]) -> [i16; 3] {
    match entry {
        Some((0, dir)) => [(-dir * 4096) as i16, 0, 0],
        Some((_, dir)) => [0, 0, (-dir * 4096) as i16],
        None => dominant_back_normal(d),
    }
}

impl<'a> GridCollision<'a> {
    /// Point trace with Quake semantics.
    #[inline(never)]
    pub(crate) fn point_trace(&self, a: [i32; 3], b: [i32; 3], body: bool) -> CollisionTrace {
        let blocked = |p: [i32; 3]| self.blocking_at(p, 0, 0, body).is_some();
        if blocked(a) {
            return solid_start(a, b, blocked(b));
        }
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let found = self.ray_first_hit(a, d);
        let (mut q, mut normal) = found.map_or((4096, [0; 3]), |h| (h.q, h.normal));
        let mut hit_found = found.is_some();
        if blocked(lerp(a, d, q)) {
            // The analytic hit and the discrete predicate disagree by a
            // rounding sliver (or a foreign room): settle on a clear sample.
            let mut hi = q;
            let mut lo = 0;
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if blocked(lerp(a, d, mid)) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            if !hit_found {
                normal = self.contact_normal(lerp(a, d, lo), lerp(a, d, hi), d, 0, 0, body);
            }
            q = lo;
            hit_found = true;
        }
        if !hit_found {
            return clear(b);
        }
        hit(lerp(a, d, q), q, normal)
    }

    /// True when nothing solid blocks the segment `a -> b`. Non-resident
    /// rooms do not block; endpoints exactly on a surface do not block.
    #[inline(never)]
    pub fn line_of_sight(&self, a: [i32; 3], b: [i32; 3]) -> bool {
        let clamp = |p: [i32; 3]| p.map(|c| c.clamp(-COORD_LIMIT, COORD_LIMIT));
        let (a, b) = (clamp(a), clamp(b));
        if self.point_in_solid(a) {
            return false;
        }
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        self.ray_first_hit(a, d).is_none()
    }

    /// First hit of the ray `a + d * q / 4096` over every resident room.
    #[inline(never)]
    fn ray_first_hit(&self, a: [i32; 3], d: [i32; 3]) -> Option<RayHit> {
        let mut best = None;
        for room in 0..self.level.rooms.len() {
            if let Some(view) = self.view(room) {
                self.ray_room(room, view, a, d, &mut best);
            }
        }
        best
    }

    #[inline(never)]
    fn ray_room(
        &self,
        room: usize,
        view: &CollisionView<'a>,
        a: [i32; 3],
        d: [i32; 3],
        best: &mut Option<RayHit>,
    ) {
        let rec = &self.level.rooms[room];
        let s = self.s;
        let (w, dp) = (view.width() as i32, view.depth() as i32);
        let (ex, ez) = (w * s, dp * s);
        let ax = a[0].wrapping_sub(rec.origin[0]);
        let az = a[2].wrapping_sub(rec.origin[2]);
        let lim = COORD_LIMIT * 2;
        if !(-lim..=lim).contains(&ax) || !(-lim..=lim).contains(&az) {
            return;
        }
        let mut qa = 0;
        let mut qb = best.map_or(4096, |b| (b.q + 1).min(4096));
        for (av, dv, ext) in [(ax, d[0], ex), (az, d[2], ez)] {
            if dv == 0 {
                if av < 0 || av >= ext {
                    return;
                }
            } else {
                let t1 = (-av) * 4096 / dv;
                let t2 = (ext - av) * 4096 / dv;
                qa = qa.max(t1.min(t2) - 1);
                qb = qb.min(t1.max(t2) + 1);
            }
        }
        if qa > qb {
            return;
        }
        let local = [ax, a[1], az];
        let p0 = lerp(local, d, qa);
        let cell = |v: i32, n: i32| self.recip.cell(v.clamp(0, n * s - 1) as u32) as i32;
        let (mut cx, mut cz) = (cell(p0[0], w), cell(p0[2], dp));
        let mut q0 = qa;
        let mut entry: Option<(usize, i32)> = None;
        loop {
            let tx = if d[0] > 0 {
                ((cx + 1) * s - ax) * 4096 / d[0]
            } else if d[0] < 0 {
                (ax - cx * s) * 4096 / -d[0]
            } else {
                i32::MAX
            };
            let tz = if d[2] > 0 {
                ((cz + 1) * s - az) * 4096 / d[2]
            } else if d[2] < 0 {
                (az - cz * s) * 4096 / -d[2]
            } else {
                i32::MAX
            };
            let q1 = tx.min(tz).min(qb).max(q0);
            self.ray_cell(
                room,
                view,
                (cx as usize, cz as usize),
                local,
                d,
                (q0, q1),
                entry,
                best,
            );
            if q1 >= qb || best.is_some_and(|b| b.q <= q1) {
                break;
            }
            if tx <= tz {
                cx += d[0].signum();
                entry = Some((0, d[0].signum()));
            }
            if tz <= tx {
                cz += d[2].signum();
                if tz < tx {
                    entry = Some((1, d[2].signum()));
                }
            }
            if cx < 0 || cx >= w || cz < 0 || cz >= dp {
                break;
            }
            q0 = q1;
        }
    }

    /// Floor (`which == 0`) or solid-ceiling (`1`) slab of a sector at a
    /// sector-local point, world `(lo, hi)`.
    fn slab_span(
        &self,
        view: &CollisionView<'a>,
        sector: &Sector,
        oy: i32,
        which: usize,
        lx: u32,
        lz: u32,
    ) -> Option<(i32, i32)> {
        if which == 0 {
            let (f, _) = self.sector_floor(view, sector, lx, lz)?;
            Some((oy + f - SLAB_THICKNESS, oy + f))
        } else if sector.flags & SECTOR_CEIL_SOLID != 0 {
            let c = self.sector_ceil(view, sector, lx, lz)?;
            Some((oy + c, oy + c + SLAB_THICKNESS))
        } else {
            None
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn ray_cell(
        &self,
        room: usize,
        view: &CollisionView<'a>,
        (cx, cz): (usize, usize),
        local: [i32; 3],
        d: [i32; 3],
        (q0, q1): (i32, i32),
        entry: Option<(usize, i32)>,
        best: &mut Option<RayHit>,
    ) {
        let s = self.s;
        let oy = self.level.rooms[room].origin[1];
        let Some(sector) = view.sectors.get(cz * view.width() + cx) else {
            return;
        };
        let (cell_x, cell_z) = (cx as i32 * s, cz as i32 * s);
        if let Some(ext) = view.ext(sector) {
            for wall in view.sector_walls(ext) {
                if let Some((q, n)) =
                    self.wall_cross(wall, local[0] - cell_x, local[1] - oy, local[2] - cell_z, d)
                {
                    consider(best, q, n);
                }
            }
        }
        let has_floor = sector.flags & SECTOR_HAS_FLOOR != 0;
        let has_ceil = sector.flags & SECTOR_CEIL_SOLID != 0 && sector.flags & SECTOR_HAS_CEIL != 0;
        if !has_floor && !has_ceil {
            return;
        }
        // World y and sector-local plan offset of the ray at `q`.
        let at = |q: i32| -> (i32, u32, u32) {
            let p = lerp(local, d, q);
            (
                p[1],
                (p[0] - cell_x).clamp(0, s) as u32,
                (p[2] - cell_z).clamp(0, s) as u32,
            )
        };
        let (_, l0x, l0z) = at(q0);
        let (_, l1x, l1z) = at(q1);
        let diag = |nesw: bool, lx: u32, lz: u32| -> i32 {
            if nesw {
                (lx + lz) as i32 - s
            } else {
                lx as i32 - lz as i32
            }
        };
        let brk = |present: bool, nesw: bool| -> i32 {
            if !present {
                return q1;
            }
            let (e0, e1) = (diag(nesw, l0x, l0z), diag(nesw, l1x, l1z));
            if (e0 < 0 && e1 > 0) || (e0 > 0 && e1 < 0) {
                q0 + (q1 - q0) * e0.abs() / (e0.abs() + e1.abs())
            } else {
                q1
            }
        };
        let mut b1 = brk(has_floor, sector.flags & SECTOR_FLOOR_NESW != 0);
        let mut b2 = brk(has_ceil, sector.flags & SECTOR_CEIL_NESW != 0);
        if b1 > b2 {
            core::mem::swap(&mut b1, &mut b2);
        }
        let pts = [q0, b1, b2, q1];
        for i in 0..3 {
            let (s0, e0) = (pts[i], pts[i + 1]);
            if (i > 0 && s0 == e0) || best.is_some_and(|b| b.q <= s0) {
                continue;
            }
            for which in 0..2 {
                if (which == 0 && !has_floor) || (which == 1 && !has_ceil) {
                    continue;
                }
                let surf = |q: i32| -> Option<(i32, i32, i32)> {
                    let (y, lx, lz) = at(q);
                    let (lo, hi) = self.slab_span(view, sector, oy, which, lx, lz)?;
                    Some((y, lo, hi))
                };
                let (Some((ys, lo_s, hi_s)), Some((ye, lo_e, hi_e))) = (surf(s0), surf(e0)) else {
                    continue;
                };
                if ys > lo_s && ys < hi_s {
                    if s0 > 0 {
                        let n = if s0 == q0 {
                            entry_normal(entry, d)
                        } else {
                            dominant_back_normal(d)
                        };
                        consider(best, s0 - 1, n);
                    }
                } else if ys >= hi_s {
                    if ye < hi_e {
                        let q = last_true(s0, e0, |q| surf(q).is_none_or(|(y, _, h)| y >= h));
                        let normal = if which == 0 {
                            let (_, lx, lz) = at(q);
                            let nesw = sector.flags & SECTOR_FLOOR_NESW != 0;
                            let second = !in_first_triangle(nesw, lx, lz, s as u32);
                            self.floor_normal(room, cx, cz, second)
                        } else {
                            [0, 4096, 0]
                        };
                        consider(best, q, normal);
                    }
                } else if ye > lo_e {
                    let q = last_true(s0, e0, |q| surf(q).is_none_or(|(y, l, _)| y <= l));
                    consider(best, q, [0, -4096, 0]);
                }
            }
        }
    }

    /// Crossing of the ray (given relative to the wall's sector, y room-local)
    /// with one wall piece: `(q, normal)` with `q` the last clear fraction.
    #[inline(never)]
    fn wall_cross(
        &self,
        wall: &ColWall,
        ax: i32,
        ay: i32,
        az: i32,
        d: [i32; 3],
    ) -> Option<(i32, [i16; 3])> {
        let s = self.s;
        let (nx, nz, k) = match wall.edge {
            edge::NORTH => (0, 1, 0),
            edge::SOUTH => (0, 1, s),
            edge::EAST => (1, 0, s),
            edge::WEST => (1, 0, 0),
            edge::NWSE => (1, -1, 0),
            edge::NESW => (1, 1, s),
            _ => return None,
        };
        let ds = nx * d[0] + nz * d[2];
        if ds == 0 {
            return None;
        }
        let side0 = nx * ax + nz * az - k;
        let side1 = side0 + ds;
        if !((side0 > 0 && side1 < 0) || (side0 < 0 && side1 > 0)) {
            return None;
        }
        let (num, den) = (side0.abs(), ds.abs());
        let x = ax + mul_div_i32(d[0], num, den);
        let z = az + mul_div_i32(d[2], num, den);
        let y = ay + mul_div_i32(d[1], num, den);
        let amount = match wall.edge {
            edge::NORTH | edge::NWSE => x,
            edge::EAST => z,
            edge::SOUTH | edge::NESW => s - x,
            _ => s - z,
        };
        if !(0..=s).contains(&amount) {
            return None;
        }
        let w = weight_q12(amount as u32, self.inv24);
        let [ba, bb, tb, ta] = wall.y.map(i32::from);
        let bottom = ba + (((bb - ba) * w) >> 12);
        let top = ta + (((tb - ta) * w) >> 12);
        if y < bottom || y > top {
            return None;
        }
        Some((
            mul_div_i32(num, 4096, den),
            wall_normal_toward(wall.edge, s, ax, az),
        ))
    }
}
