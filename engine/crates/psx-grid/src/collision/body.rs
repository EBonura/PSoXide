//! Body traces: analytic vertical sweeps and marched horizontal sweeps.
//!
//! Every function takes the query by reference and writes into one
//! caller-owned [`Raw`], so frames stay small (see `scan.rs`).

use super::scan::{kind, nc, Scan, Src};
use super::world::{GridCollision, SLAB_THICKNESS};

/// Most marching samples one body trace may take. Longer sweeps widen the
/// step instead (they are not issued by the motor).
const MAX_STEPS: i32 = 2048;
/// Bisection iterations: enough to resolve a Q12 fraction.
const BISECT: u32 = 12;

/// One trace request in world units. `d = end - start`.
pub(crate) struct Query {
    pub a: [i32; 3],
    pub d: [i32; 3],
    pub r: i16,
    pub h: i16,
    /// A `Shape::Body` request: non-resident rooms are solid.
    pub body: bool,
}

impl Query {
    /// Radius, runtime units.
    #[inline(always)]
    pub fn r(&self) -> i32 {
        i32::from(self.r)
    }

    /// Height, runtime units.
    #[inline(always)]
    pub fn h(&self) -> i32 {
        i32::from(self.h)
    }

    #[inline(always)]
    pub fn at(&self, q: i32) -> [i32; 3] {
        lerp(self.a, self.d, q)
    }

    /// Height only.
    #[inline(always)]
    pub fn y_at(&self, q: i32) -> i32 {
        self.a[1] + self.d[1] * q / 4096
    }
}

/// No end-height override.
pub(crate) const NO_Y: i32 = i32::MIN;

/// Raw trace result; the provider expands it into a `CollisionTrace`.
///
/// `end` is `a + d * fraction / 4096` unless `end_y` overrides the height (an
/// analytic vertical hit lands exactly on the surface). `contact_hi != 0`
/// asks the caller to derive the normal with [`GridCollision::contact`] for
/// the move `fraction -> contact_hi`. The point tracer also uses `fraction`
/// (above 4096 meaning none), `normal` and `src` as its earliest-hit
/// accumulator.
pub(crate) struct Raw {
    pub fraction: i32,
    pub end_y: i32,
    pub contact_hi: i16,
    pub start_solid: bool,
    pub all_solid: bool,
    pub normal: u8,
    pub src: Src,
}

impl Raw {
    pub fn new() -> Self {
        Self {
            fraction: 4096,
            end_y: NO_Y,
            contact_hi: 0,
            start_solid: false,
            all_solid: false,
            normal: nc::NONE,
            src: Src::default(),
        }
    }
}

#[inline(always)]
pub(crate) fn lerp(a: [i32; 3], d: [i32; 3], q: i32) -> [i32; 3] {
    // Division truncates toward zero, i.e. toward the start of the segment.
    [
        a[0] + d[0] * q / 4096,
        a[1] + d[1] * q / 4096,
        a[2] + d[2] * q / 4096,
    ]
}

/// Quake-style result for a start that is already inside solid.
pub(crate) fn solid_start(raw: &mut Raw, all_solid: bool) {
    raw.start_solid = true;
    if all_solid {
        raw.all_solid = true;
        raw.fraction = 0;
    }
}

/// Horizontal normal opposing the dominant horizontal motion.
pub(crate) fn back_code(d: &[i32; 3]) -> u8 {
    if d[0].abs() >= d[2].abs() {
        if d[0] < 0 {
            nc::PX
        } else {
            nc::NX
        }
    } else if d[2] < 0 {
        nc::PZ
    } else {
        nc::NZ
    }
}

impl<'a> GridCollision<'a> {
    /// Dispatch a trace. `q.r()` must already be clamped to `0..=S`.
    #[inline(always)]
    pub(crate) fn trace(&self, q: &Query, raw: &mut Raw) {
        if q.d[0] == 0 && q.d[2] == 0 {
            self.vertical_trace(q, raw);
        } else if q.r() == 0 && q.h() == 0 {
            self.point_trace(q, raw);
        } else {
            self.march_trace(q, raw);
        }
    }

    /// Is the cylinder at `q.at(t)` inside solid?
    #[inline(never)]
    pub(crate) fn blocked_t(&self, q: &Query, t: i32) -> bool {
        if q.r() == 0 {
            return self.point_blocked(&q.at(t), q.h(), q.body);
        }
        let mut scan = Scan::any(q.at(t), q.r(), q.h(), q.body);
        self.scan_column(&mut scan);
        scan.found
    }

    /// Pure vertical sweep: every blocking interval is collected once and the
    /// first one along the path wins. Exact for any length.
    #[inline(never)]
    pub(crate) fn vertical_trace(&self, q: &Query, raw: &mut Raw) {
        let a = q.a;
        let y1 = a[1] + q.d[1];
        let mut scan = Scan::sweep(a, y1, q.r(), q.h(), q.body);
        self.scan_column(&mut scan);
        if scan.start_in {
            solid_start(raw, scan.end_in);
            return;
        }
        if !scan.found {
            return;
        }
        let span = (a[1] - y1).abs();
        if y1 < a[1] {
            raw.fraction = (a[1] - scan.hi) * 4096 / span.max(1);
            raw.end_y = scan.hi;
            if scan.src.kind == kind::FLOOR {
                raw.normal = nc::FLOOR;
                raw.src = scan.src;
            } else if scan.src.kind == kind::PLANE
                && scan.src.aux == crate::format::plane_kind::PLATFORM
            {
                raw.normal = nc::PLATFORM_TOP;
                raw.src = scan.src;
            } else {
                raw.normal = nc::UP;
            }
        } else {
            raw.fraction = (scan.lo - a[1]) * 4096 / span.max(1);
            raw.end_y = scan.lo;
            raw.normal = nc::DOWN;
        }
    }

    /// Horizontal or diagonal body sweep by marching and bisecting.
    #[inline(never)]
    pub(crate) fn march_trace(&self, q: &Query, raw: &mut Raw) {
        if self.blocked_t(q, 0) {
            let all = self.blocked_t(q, 4096);
            solid_start(raw, all);
            return;
        }
        let len = q.d[0].abs().max(q.d[1].abs()).max(q.d[2].abs());
        let step = q
            .r()
            .max(2)
            .min((self.s / 4).max(2).min(SLAB_THICKNESS))
            .max(1);
        let n = ((len + step - 1) / step).max(1).min(MAX_STEPS);
        let mut lo = 0;
        let mut hi = 0;
        for i in 1..=n {
            let t = if i == n { 4096 } else { i * 4096 / n };
            if self.blocked_t(q, t) {
                hi = t;
                break;
            }
            lo = t;
        }
        if hi == 0 {
            return;
        }
        // One world unit of travel is plenty: the motor only reads whether the
        // sweep hit, and the end lands within a unit of the surface.
        for _ in 0..BISECT {
            if hi - lo <= 1 || (hi - lo) * len <= 4096 {
                break;
            }
            let mid = (lo + hi) / 2;
            if self.blocked_t(q, mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        raw.fraction = lo;
        raw.contact_hi = hi as i16;
    }

    /// Normal of the surface that blocked a cylinder moving `lo -> hi`.
    #[inline(never)]
    pub(crate) fn contact(&self, q: &Query, lo: i32, hi: i32, raw: &mut Raw) {
        let mut scan = Scan::any(q.at(hi), q.r(), q.h(), q.body);
        self.scan_column(&mut scan);
        self.classify(q, lo, hi, &scan, raw);
    }

    #[inline(never)]
    fn classify(&self, q: &Query, lo: i32, hi: i32, scan: &Scan, raw: &mut Raw) {
        let d = &q.d;
        if !scan.found {
            raw.normal = back_code(d);
            return;
        }
        let from = q.at(lo);
        let src = scan.src;
        match src.kind {
            kind::WALL => {
                let rec = self.rec(usize::from(src.room));
                let ox = rec.origin[0] + i32::from(src.cx) * self.s;
                let oz = rec.origin[2] + i32::from(src.cz) * self.s;
                raw.normal = super::world::wall_code(src.aux, self.s, from[0] - ox, from[2] - oz);
            }
            kind::FLOOR => {
                let top = scan.hi;
                if from[1] >= top {
                    raw.normal = nc::FLOOR;
                    raw.src = src;
                } else if from[1] + q.h() <= top - SLAB_THICKNESS {
                    raw.normal = nc::DOWN;
                } else {
                    let at = q.at(hi);
                    let rec = self.rec(usize::from(src.room));
                    let cell = |v: i32, o: i32| (v - o).div_euclid(self.s.max(1));
                    let cx = cell(from[0], rec.origin[0]) != cell(at[0], rec.origin[0]);
                    let cz = cell(from[2], rec.origin[2]) != cell(at[2], rec.origin[2]);
                    raw.normal = match (cx, cz) {
                        (false, false) => {
                            raw.src = src;
                            nc::FLOOR
                        }
                        (true, false) => {
                            if d[0] < 0 {
                                nc::PX
                            } else {
                                nc::NX
                            }
                        }
                        (false, true) => {
                            if d[2] < 0 {
                                nc::PZ
                            } else {
                                nc::NZ
                            }
                        }
                        (true, true) => back_code(d),
                    };
                }
            }
            kind::PLANE => self.classify_plane(q, lo, scan, raw),
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
}
