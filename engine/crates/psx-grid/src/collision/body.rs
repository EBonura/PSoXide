//! Body traces: analytic vertical sweeps and marched horizontal sweeps.

use super::world::{wall_normal_toward, Block, GridCollision, Source, SLAB_THICKNESS};
use psx_engine::{CollisionTrace, RoomPoint};

/// Most marching samples one body trace may take. Longer sweeps widen the
/// step instead (they are not issued by the motor).
const MAX_STEPS: i32 = 2048;
/// Bisection iterations: enough to resolve a Q12 fraction.
const BISECT: u32 = 12;

#[inline]
pub(crate) fn lerp(a: [i32; 3], d: [i32; 3], q: i32) -> [i32; 3] {
    // Division truncates toward zero, i.e. toward the start of the segment.
    [
        a[0] + d[0] * q / 4096,
        a[1] + d[1] * q / 4096,
        a[2] + d[2] * q / 4096,
    ]
}

/// Quake-style result for a start that is already inside solid.
pub(crate) fn solid_start(a: [i32; 3], b: [i32; 3], all_solid: bool) -> CollisionTrace {
    if all_solid {
        CollisionTrace {
            all_solid: true,
            start_solid: true,
            fraction_q12: 0,
            end: RoomPoint::from_array(a),
            ..CollisionTrace::default()
        }
    } else {
        CollisionTrace {
            start_solid: true,
            fraction_q12: 4096,
            end: RoomPoint::from_array(b),
            ..CollisionTrace::default()
        }
    }
}

pub(crate) fn clear(b: [i32; 3]) -> CollisionTrace {
    CollisionTrace::unobstructed(RoomPoint::from_array(b))
}

pub(crate) fn hit(end: [i32; 3], fraction: i32, normal: [i16; 3]) -> CollisionTrace {
    CollisionTrace {
        fraction_q12: fraction,
        end: RoomPoint::from_array(end),
        normal_q12: normal,
        ..CollisionTrace::default()
    }
}

fn sign(v: i32) -> i32 {
    if v < 0 {
        -1
    } else {
        1
    }
}

/// Horizontal normal opposing the dominant horizontal motion.
pub(crate) fn dominant_back_normal(d: [i32; 3]) -> [i16; 3] {
    if d[0].abs() >= d[2].abs() {
        [(-sign(d[0]) * 4096) as i16, 0, 0]
    } else {
        [0, 0, (-sign(d[2]) * 4096) as i16]
    }
}

impl<'a> GridCollision<'a> {
    /// Trace a cylinder (or a point when `radius == 0 && height == 0`) from
    /// `a` to `b`. Coordinates must be within `COORD_LIMIT`.
    #[inline(never)]
    pub(crate) fn trace(
        &self,
        a: [i32; 3],
        b: [i32; 3],
        radius: i32,
        height: i32,
        body: bool,
    ) -> CollisionTrace {
        let radius = radius.clamp(0, self.s);
        if a[0] == b[0] && a[2] == b[2] {
            self.vertical_trace(a, b, radius, height, body)
        } else if radius == 0 && height == 0 {
            self.point_trace(a, b, body)
        } else {
            self.march_trace(a, b, radius, height, body)
        }
    }

    /// Pure vertical sweep: every blocking interval is collected once and the
    /// first one along the path wins. Exact for any length.
    #[inline(never)]
    fn vertical_trace(
        &self,
        a: [i32; 3],
        b: [i32; 3],
        r: i32,
        h: i32,
        body: bool,
    ) -> CollisionTrace {
        let (y0, y1) = (a[1], b[1]);
        let mut start_in = false;
        let mut end_in = false;
        let mut best: Option<Block> = None;
        self.visit_blocks(a[0], a[2], r, h, body, &mut |bl| {
            start_in |= bl.lo < y0 && y0 < bl.hi;
            end_in |= bl.lo < y1 && y1 < bl.hi;
            if y1 < y0 {
                if bl.hi <= y0 && bl.hi > y1 && best.is_none_or(|x| bl.hi > x.hi) {
                    best = Some(bl);
                }
            } else if y1 > y0 && bl.lo >= y0 && bl.lo < y1 && best.is_none_or(|x| bl.lo < x.lo) {
                best = Some(bl);
            }
            false
        });
        if start_in {
            return solid_start(a, b, end_in);
        }
        let Some(bl) = best else {
            return clear(b);
        };
        let span = (y0 - y1).abs();
        if y1 < y0 {
            let fraction = (y0 - bl.hi) * 4096 / span;
            let normal = match bl.src {
                Source::Floor {
                    room,
                    cx,
                    cz,
                    second,
                } => self.floor_normal(room, cx, cz, second),
                _ => [0, 4096, 0],
            };
            hit([a[0], bl.hi, a[2]], fraction, normal)
        } else {
            let fraction = (bl.lo - y0) * 4096 / span;
            hit([a[0], bl.lo, a[2]], fraction, [0, -4096, 0])
        }
    }

    /// Horizontal or diagonal body sweep by marching and bisecting.
    #[inline(never)]
    fn march_trace(&self, a: [i32; 3], b: [i32; 3], r: i32, h: i32, body: bool) -> CollisionTrace {
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let blocked = |p: [i32; 3]| self.blocking_at(p, r, h, body).is_some();
        let start_blocked = blocked(a);
        let end_blocked = blocked(b);
        if start_blocked {
            return solid_start(a, b, end_blocked);
        }
        let len = d[0].abs().max(d[1].abs()).max(d[2].abs());
        let step = r.clamp(2, (self.s / 4).clamp(2, SLAB_THICKNESS));
        let n = ((len + step - 1) / step).clamp(1, MAX_STEPS);
        let mut prev = 0;
        let mut found = None;
        for i in 1..=n {
            let q = if i == n { 4096 } else { i * 4096 / n };
            if blocked(lerp(a, d, q)) {
                found = Some((prev, q));
                break;
            }
            prev = q;
        }
        let Some((mut lo, mut hi)) = found else {
            return clear(b);
        };
        for _ in 0..BISECT {
            if hi - lo <= 1 {
                break;
            }
            let mid = (lo + hi) / 2;
            if blocked(lerp(a, d, mid)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let from = lerp(a, d, lo);
        let at = lerp(a, d, hi);
        let normal = self.contact_normal(from, at, d, r, h, body);
        hit(from, lo, normal)
    }

    /// Normal of the surface that blocked a body moving `from -> at`.
    #[inline(never)]
    pub(crate) fn contact_normal(
        &self,
        from: [i32; 3],
        at: [i32; 3],
        d: [i32; 3],
        r: i32,
        h: i32,
        body: bool,
    ) -> [i16; 3] {
        let Some(bl) = self.blocking_at(at, r, h, body) else {
            return dominant_back_normal(d);
        };
        match bl.src {
            Source::Wall { edge, ox, oz } => {
                wall_normal_toward(edge, self.s, from[0] - ox, from[2] - oz)
            }
            Source::Ceil | Source::Foreign => {
                if d[1] > 0 && from[1] + h <= bl.lo + h {
                    [0, -4096, 0]
                } else if d[1] < 0 && d[1].abs() >= d[0].abs().max(d[2].abs()) {
                    [0, 4096, 0]
                } else {
                    dominant_back_normal(d)
                }
            }
            Source::Floor {
                room,
                cx,
                cz,
                second,
            } => {
                let top = bl.hi;
                if from[1] >= top {
                    self.floor_normal(room, cx, cz, second)
                } else if from[1] + h <= top - SLAB_THICKNESS {
                    [0, -4096, 0]
                } else {
                    let rec = &self.level.rooms[room];
                    let cell = |v: i32, o: i32| (v - o).div_euclid(self.s);
                    let cx_changed = cell(from[0], rec.origin[0]) != cell(at[0], rec.origin[0]);
                    let cz_changed = cell(from[2], rec.origin[2]) != cell(at[2], rec.origin[2]);
                    match (cx_changed, cz_changed) {
                        (false, false) => self.floor_normal(room, cx, cz, second),
                        (true, false) => [(-sign(d[0]) * 4096) as i16, 0, 0],
                        (false, true) => [0, 0, (-sign(d[2]) * 4096) as i16],
                        (true, true) => dominant_back_normal(d),
                    }
                }
            }
        }
    }
}
