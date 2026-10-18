//! Free-plane collision: platforms, walls and blocker boxes.
//!
//! A room's `COL_PLANES` table holds one record per collidable free plane and
//! per overlapped cell (see [`ColPlane`] for the three shapes). The three
//! queries here mirror what the sector slabs offer:
//!
//! * body scans ([`GridCollision::scan_planes`]) add forbidden feet-height
//!   intervals to the column scan, exactly like a floor slab, a ceiling slab
//!   or a wall piece, so the character motor and the camera arm see planes
//!   through the same provider contract;
//! * the cheap point predicate ([`GridCollision::plane_blocks_point`]) backs
//!   `point_blocked`;
//! * point rays ([`GridCollision::ray_planes`]) find the first crossing of a
//!   segment with any plane, with a normal, for point traces and line of
//!   sight.
//!
//! Everything is `i32` integer arithmetic. Plane sizes are capped by the cook
//! (at most a few hundred runtime units across), so every product stays far
//! inside `i32` for positions near a plane; rays are clipped by the provider
//! to [`COORD_LIMIT`](super::COORD_LIMIT).

use super::body::{back_code, Query, Raw};
use super::ray::consider;
use super::scan::{kind, nc, Scan, Src};
use super::world::GridCollision;
use crate::format::*;
use psx_math::int32::{isqrt_i32, mul_div_i32};

/// [`platform_top`] result for a point outside the footprint.
const NO_TOP: i32 = i32::MIN;

/// Surface height of a platform at room-local `(x, z)`, or [`NO_TOP`] outside
/// its footprint. A leaf, so the deep scan chain pays for it only briefly.
#[inline(never)]
fn platform_top(plane: &ColPlane, x: i32, z: i32) -> i32 {
    if in_footprint(plane, x, z) {
        platform_y(plane, x, z)
    } else {
        NO_TOP
    }
}

/// Surface height of a platform at room-local `(x, z)`.
#[inline(always)]
pub(crate) fn platform_y(plane: &ColPlane, x: i32, z: i32) -> i32 {
    let dx = x - i32::from(plane.xz[0][0]);
    let dz = z - i32::from(plane.xz[0][1]);
    i32::from(plane.y0) + ((i32::from(plane.sx_q12) * dx + i32::from(plane.sz_q12) * dz) >> 12)
}

/// Cross product of footprint edge `i` (a to b) with the vector to `(x, z)`:
/// not negative when the point is on the inside of it.
#[inline(always)]
fn edge_side(plane: &ColPlane, i: usize, x: i32, z: i32) -> i32 {
    let a = plane.xz[i];
    let b = plane.xz[(i + 1) & 3];
    (i32::from(b[0]) - i32::from(a[0])) * (z - i32::from(a[1]))
        - (i32::from(b[1]) - i32::from(a[1])) * (x - i32::from(a[0]))
}

/// Whether `(x, z)` is inside or on the edge of a platform's footprint. The
/// cook writes the ring so that every edge test is `>= 0` inside.
#[inline(always)]
pub(crate) fn in_footprint(plane: &ColPlane, x: i32, z: i32) -> bool {
    (0..4).all(|i| edge_side(plane, i, x, z) >= 0)
}

/// Whether the wall segment is closer than `r` to `(x, z)` (strict).
#[inline(always)]
fn wall_within(plane: &ColPlane, x: i32, z: i32, r: i32) -> bool {
    let (a, b) = (plane.xz[0], plane.xz[1]);
    let (dx, dz) = (i32::from(plane.sx_q12), i32::from(plane.sz_q12));
    let (px, pz) = (x - i32::from(a[0]), z - i32::from(a[1]));
    let along = (px * dx + pz * dz) >> 12;
    let length =
        ((i32::from(b[0]) - i32::from(a[0])) * dx + (i32::from(b[1]) - i32::from(a[1])) * dz) >> 12;
    let r2 = r * r;
    if along < 0 {
        px * px + pz * pz < r2
    } else if along > length {
        let (qx, qz) = (x - i32::from(b[0]), z - i32::from(b[1]));
        qx * qx + qz * qz < r2
    } else {
        let across = (px * dz - pz * dx) >> 12;
        across * across < r2
    }
}

/// Whether the blocker box is closer than `r` to `(x, z)`; with `r == 0`,
/// whether the point is strictly inside.
#[inline(always)]
fn box_within(plane: &ColPlane, x: i32, z: i32, r: i32) -> bool {
    let (lo, hi) = (plane.xz[0], plane.xz[1]);
    let gap = |v: i32, lo: i16, hi: i16| (i32::from(lo) - v).max(v - i32::from(hi)).max(0);
    let (gx, gz) = (gap(x, lo[0], hi[0]), gap(z, lo[1], hi[1]));
    if r == 0 {
        x > i32::from(lo[0]) && x < i32::from(hi[0]) && z > i32::from(lo[1]) && z < i32::from(hi[1])
    } else {
        gx * gx + gz * gz < r * r
    }
}

/// Table index named by a plane hit's source.
#[inline(always)]
fn src_index(src: &Src) -> usize {
    usize::from(src.cx) | usize::from(src.cz) << 8
}

fn plane_src(room: usize, index: usize, plane_kind: u8, aux: u8) -> Src {
    Src {
        kind: kind::PLANE,
        aux: if aux != 0 { aux } else { plane_kind },
        cx: index as u8,
        cz: (index >> 8) as u8,
        room: room as u16,
        span: 0,
    }
}

/// Running intersection of a ray with an open convex prism, one half-space
/// at a time (Cyrus-Beck). A half-space is given by its signed distance at
/// the ray start and end, positive inside; touching a face does not count as
/// inside, so a body standing on a surface is clear of it.
struct Prism {
    enter: i32,
    leave: i32,
    face: u8,
    miss: bool,
}

impl Prism {
    fn new() -> Self {
        Self {
            enter: 0,
            leave: 4096,
            face: 0,
            miss: false,
        }
    }

    #[inline(always)]
    fn limit(&mut self, u0: i32, u1: i32, face: u8) {
        if u0 > 0 && u1 > 0 {
            return;
        }
        if u0 <= 0 && u1 <= 0 {
            self.miss = true;
            return;
        }
        let t = mul_div_i32(4096, u0.abs(), (u1 - u0).abs());
        if u0 <= 0 {
            if t >= self.enter {
                self.enter = t;
                self.face = face;
            }
        } else if t < self.leave {
            self.leave = t;
        }
    }

    /// Whether any part of the ray is inside.
    fn hit(&self) -> bool {
        !self.miss && self.enter < self.leave
    }
}

impl<'a> GridCollision<'a> {
    /// Offer the intervals of every plane near the scan's column. Called for
    /// every room whose plan the cylinder may touch; cheap for rooms
    /// without planes.
    #[inline(never)]
    pub(crate) fn scan_planes(&self, room: usize, scan: &mut Scan) {
        let Some(view) = self.view(room) else {
            return;
        };
        if view.col_planes.is_empty() {
            return;
        }
        let rec = self.rec(room);
        let (s, r) = (self.s, scan.r);
        let (w, d) = (i32::from(rec.width), i32::from(rec.depth));
        let (lx, lz) = (scan.x - rec.origin[0], scan.z - rec.origin[2]);
        if lx + r < 0 || lz + r < 0 || lx - r >= w * s || lz - r >= d * s {
            return;
        }
        let recip = self.recip;
        let cell = |v: i32, n: i32| recip.cell(v.max(0).min(n * s - 1) as u32) as usize;
        let (cx0, cx1) = (cell(lx - r, w), cell(lx + r, w));
        let (cz0, cz1) = (cell(lz - r, d), cell(lz + r, d));
        scan.oy = rec.origin[1];
        scan.room = room as u16;
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                // One record per sector in a dense room; in a tree room the
                // leaves that fill the sector (planes are copied into every
                // leaf they touch, in room coordinates, so any leaf's list
                // serves).
                for slot in 0..Self::cell_slots(view, cx, cz) {
                    let Some(leaf) = self.cell_leaf(view, cx, cz, slot) else {
                        continue;
                    };
                    let Some(ext) = view.ext(leaf.rec) else {
                        continue;
                    };
                    let first = usize::from(ext.plane_first);
                    for (k, plane) in view.sector_planes(ext).iter().enumerate() {
                        if self.camera_pass && plane.flags & COLPLANE_CAMERA_PASS != 0 {
                            continue;
                        }
                        Self::offer_plane(plane, first + k, lx, lz, scan);
                    }
                }
            }
        }
    }

    /// Offer one plane's interval. Associated, with the room and height
    /// offset read back from the scan: the motor runs this on a 1 KiB stack
    /// and every argument spilled here is stack on the deepest path.
    #[inline(never)]
    fn offer_plane(plane: &ColPlane, index: usize, lx: i32, lz: i32, scan: &mut Scan) {
        let src = plane_src(usize::from(scan.room), index, plane.kind, 0);
        match plane.kind {
            plane_kind::PLATFORM => Self::offer_platform(plane, src, lx, lz, scan),
            plane_kind::WALL => {
                if scan.r > 0 && wall_within(plane, lx, lz, scan.r) {
                    Self::offer_span(plane, src, scan);
                }
            }
            _ => {
                if box_within(plane, lx, lz, scan.r) {
                    Self::offer_span(plane, src, scan);
                }
            }
        }
    }

    /// The vertical extent of a wall or blocker as a forbidden interval.
    #[inline(never)]
    fn offer_span(plane: &ColPlane, src: Src, scan: &mut Scan) {
        let bottom = scan.oy + i32::from(plane.y0);
        scan.offer(bottom - scan.h, bottom + i32::from(plane.extent), src);
    }

    /// A platform's slab under each footprint sample inside it.
    #[inline(never)]
    fn offer_platform(plane: &ColPlane, src: Src, lx: i32, lz: i32, scan: &mut Scan) {
        let (ox, oz) = (scan.x - lx, scan.z - lz);
        for i in 0..if scan.r > 0 { 5 } else { 1 } {
            let (sx, sz) = scan.sample(i);
            let top = platform_top(plane, sx - ox, sz - oz);
            if top != NO_TOP {
                let top = scan.oy + top;
                scan.offer(top - i32::from(plane.extent) - scan.h, top, src);
            }
        }
    }

    /// Whether a point (or an upright body of height `h` over one sample) at
    /// room-local plan `(lx, lz)` and world height `y` is inside a plane of
    /// the sector `sector` of `view`.
    #[inline(never)]
    pub(crate) fn plane_blocks_point(
        view: &crate::view::CollisionView<'a>,
        sector: &Sector,
        oy: i32,
        (lx, lz): (i32, i32),
        y: i32,
        h: i32,
        camera_pass: bool,
    ) -> bool {
        let Some(ext) = view.ext(sector) else {
            return false;
        };
        for plane in view.sector_planes(ext) {
            if camera_pass && plane.flags & COLPLANE_CAMERA_PASS != 0 {
                continue;
            }
            let bottom = oy + i32::from(plane.y0);
            let blocked = match plane.kind {
                plane_kind::PLATFORM => {
                    let top = oy + platform_y(plane, lx, lz);
                    in_footprint(plane, lx, lz) && y > top - i32::from(plane.extent) - h && y < top
                }
                plane_kind::BLOCKER => {
                    box_within(plane, lx, lz, 0)
                        && y > bottom - h
                        && y < bottom + i32::from(plane.extent)
                }
                _ => false,
            };
            if blocked {
                return true;
            }
        }
        false
    }

    /// Fold the first crossing of the ray with any plane of `room` into
    /// `hit` (`consider` keeps the earliest).
    #[inline(never)]
    pub(crate) fn ray_planes(&self, room: usize, q: &Query, hit: &mut Raw) {
        let Some(view) = self.view(room) else {
            return;
        };
        if view.col_planes.is_empty() {
            return;
        }
        let rec = self.rec(room);
        let a = [
            q.a[0] - rec.origin[0],
            q.a[1] - rec.origin[1],
            q.a[2] - rec.origin[2],
        ];
        let b = [a[0] + q.d[0], a[1] + q.d[1], a[2] + q.d[2]];
        for (index, plane) in view.col_planes.iter().enumerate() {
            if plane.flags & COLPLANE_PRIMARY == 0 {
                continue;
            }
            if self.camera_pass && plane.flags & COLPLANE_CAMERA_PASS != 0 {
                continue;
            }
            match plane.kind {
                plane_kind::WALL => self.ray_wall(room, index, plane, &a, &b, q, hit),
                plane_kind::PLATFORM => self.ray_platform(room, index, plane, &a, &b, q, hit),
                _ => self.ray_blocker(room, index, plane, &a, &b, hit),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ray_wall(
        &self,
        room: usize,
        index: usize,
        plane: &ColPlane,
        a: &[i32; 3],
        b: &[i32; 3],
        q: &Query,
        hit: &mut Raw,
    ) {
        let (p0, p1) = (plane.xz[0], plane.xz[1]);
        let (dx, dz) = (i32::from(plane.sx_q12), i32::from(plane.sz_q12));
        let (x0, z0) = (i32::from(p0[0]), i32::from(p0[1]));
        let side = |p: &[i32; 3]| (p[0] - x0) * dz - (p[2] - z0) * dx;
        let (s0, s1) = (side(a), side(b));
        if !((s0 > 0 && s1 < 0) || (s0 < 0 && s1 > 0)) {
            return;
        }
        let t = mul_div_i32(4096, s0.abs(), s0.abs() + s1.abs());
        let at = |i: usize| a[i] + mul_div_i32(q.d[i], t, 4096);
        let (x, y, z) = (at(0), at(1), at(2));
        let along = ((x - x0) * dx + (z - z0) * dz) >> 12;
        let length = ((i32::from(p1[0]) - x0) * dx + (i32::from(p1[1]) - z0) * dz) >> 12;
        let bottom = i32::from(plane.y0);
        if along < 0 || along > length || y < bottom || y > bottom + i32::from(plane.extent) {
            return;
        }
        let src = plane_src(room, index, plane.kind, u8::from(s0 > 0) + 4);
        consider(hit, t, nc::WALL_PLANE, src);
    }

    #[allow(clippy::too_many_arguments)]
    fn ray_platform(
        &self,
        room: usize,
        index: usize,
        plane: &ColPlane,
        a: &[i32; 3],
        b: &[i32; 3],
        q: &Query,
        hit: &mut Raw,
    ) {
        let mut prism = Prism::new();
        for i in 0..4 {
            // The footprint is closed (`in_footprint`): inside is `>= 0`.
            prism.limit(
                edge_side(plane, i, a[0], a[2]) + 1,
                edge_side(plane, i, b[0], b[2]) + 1,
                8,
            );
        }
        let thickness = i32::from(plane.extent);
        let (top_a, top_b) = (platform_y(plane, a[0], a[2]), platform_y(plane, b[0], b[2]));
        prism.limit(top_a - a[1], top_b - b[1], 1);
        prism.limit(a[1] - top_a + thickness, b[1] - top_b + thickness, 2);
        let code = match prism.face {
            1 => nc::PLATFORM_TOP,
            2 => nc::DOWN,
            _ => back_code(&q.d),
        };
        self.prism_result(room, index, plane, &prism, code, hit);
    }

    #[allow(clippy::too_many_arguments)]
    fn ray_blocker(
        &self,
        room: usize,
        index: usize,
        plane: &ColPlane,
        a: &[i32; 3],
        b: &[i32; 3],
        hit: &mut Raw,
    ) {
        let (lo, hi) = (plane.xz[0], plane.xz[1]);
        let bottom = i32::from(plane.y0);
        let top = bottom + i32::from(plane.extent);
        let mut prism = Prism::new();
        prism.limit(a[0] - i32::from(lo[0]), b[0] - i32::from(lo[0]), 3);
        prism.limit(i32::from(hi[0]) - a[0], i32::from(hi[0]) - b[0], 4);
        prism.limit(a[2] - i32::from(lo[1]), b[2] - i32::from(lo[1]), 5);
        prism.limit(i32::from(hi[1]) - a[2], i32::from(hi[1]) - b[2], 6);
        prism.limit(a[1] - bottom, b[1] - bottom, 2);
        prism.limit(top - a[1], top - b[1], 1);
        let code = match prism.face {
            1 => nc::UP,
            2 => nc::DOWN,
            3 => nc::NX,
            4 => nc::PX,
            5 => nc::NZ,
            6 => nc::PZ,
            _ => nc::NONE,
        };
        self.prism_result(room, index, plane, &prism, code, hit);
    }

    fn prism_result(
        &self,
        room: usize,
        index: usize,
        plane: &ColPlane,
        prism: &Prism,
        code: u8,
        hit: &mut Raw,
    ) {
        if !prism.hit() {
            return;
        }
        if prism.face == 0 {
            // No entering face: the ray starts inside.
            hit.start_solid = true;
            return;
        }
        let src = plane_src(room, index, plane.kind, 0);
        consider(hit, prism.enter, code, src);
    }

    /// Q12 normal of a platform's top face.
    pub(crate) fn platform_normal(&self, src: &Src) -> [i16; 3] {
        let up = [0, 4096, 0];
        let Some(plane) = self
            .view(usize::from(src.room))
            .and_then(|v| v.col_planes.get(src_index(src)))
        else {
            return up;
        };
        let (sx, sz) = (i32::from(plane.sx_q12), i32::from(plane.sz_q12));
        let length = isqrt_i32(sx * sx + sz * sz + 4096 * 4096).max(1);
        let q = |v: i32| mul_div_i32(v, 4096, length) as i16;
        [q(-sx), q(4096), q(-sz)]
    }

    /// Q12 normal of a wall plane, facing the side the hit came from
    /// (`src.aux` is 5 for the positive side, 4 for the negative one).
    pub(crate) fn wall_plane_normal(&self, src: &Src) -> [i16; 3] {
        let Some(plane) = self
            .view(usize::from(src.room))
            .and_then(|v| v.col_planes.get(src_index(src)))
        else {
            return [0; 3];
        };
        let (dx, dz) = (plane.sx_q12, plane.sz_q12);
        if src.aux == 5 {
            [dz, 0, -dx]
        } else {
            [-dz, 0, dx]
        }
    }

    /// Which side of a wall plane the plan point `(x, z)` (room-local) is on:
    /// the `aux` a hit source needs.
    pub(crate) fn wall_side(&self, src: &Src, x: i32, z: i32) -> u8 {
        let Some(plane) = self
            .view(usize::from(src.room))
            .and_then(|v| v.col_planes.get(src_index(src)))
        else {
            return 4;
        };
        let a = plane.xz[0];
        let side = (x - i32::from(a[0])) * i32::from(plane.sz_q12)
            - (z - i32::from(a[1])) * i32::from(plane.sx_q12);
        if side > 0 {
            5
        } else {
            4
        }
    }

    /// Contact normal of a body that was stopped by the plane named by
    /// `scan.src` while moving `lo -> hi` (Q12 fractions of `q`).
    #[inline(never)]
    pub(crate) fn classify_plane(&self, q: &Query, lo: i32, scan: &Scan, raw: &mut Raw) {
        let d = &q.d;
        let from = q.at(lo);
        let src = scan.src;
        let Some(plane) = self.plane_of(&src) else {
            raw.normal = back_code(d);
            return;
        };
        match plane.kind {
            plane_kind::WALL => {
                let rec = self.rec(usize::from(src.room));
                let side = self.wall_side(&src, from[0] - rec.origin[0], from[2] - rec.origin[2]);
                raw.normal = nc::WALL_PLANE;
                raw.src = Src { aux: side, ..src };
            }
            plane_kind::PLATFORM => {
                let top = scan.hi;
                if from[1] >= top {
                    raw.normal = nc::PLATFORM_TOP;
                    raw.src = src;
                } else if from[1] + q.h() <= top - i32::from(plane.extent) {
                    raw.normal = nc::DOWN;
                } else {
                    raw.normal = back_code(d);
                }
            }
            _ => {
                raw.normal = if d[1] > 0 {
                    nc::DOWN
                } else if d[1] < 0 && d[1].abs() >= d[0].abs().max(d[2].abs()) {
                    nc::UP
                } else {
                    back_code(d)
                };
            }
        }
    }

    /// The plane record a hit source names.
    pub(crate) fn plane_of(&self, src: &Src) -> Option<&'a ColPlane> {
        self.view(usize::from(src.room))
            .and_then(|v| v.col_planes.get(src_index(src)))
    }
}
