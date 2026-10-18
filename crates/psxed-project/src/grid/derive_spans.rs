//! Derived walls on an edge where one side or both hold more than one span
//! (`docs/grid/multi-storey.md`, 4.4).
//!
//! A column's **open set** along the edge is the union of its spans' open
//! intervals `[floor, ceiling]`; the solid is everything between them. A face
//! sits wherever a point is open on one side and solid on the other: it is
//! seen from the open side (its host) and owned by the solid side. Each
//! piece is the strip between one open interval and one solid gap, and the
//! two bounds that close it say what it is:
//!
//! * floor of the open side below, floor of the solid side above: **riser**
//!   (R1), owned by the solid span whose floor closes the gap;
//! * ceiling of the solid side below, ceiling of the open side above: **drop**
//!   (R2), owned by the solid span whose ceiling opens the gap;
//! * ceiling of the solid side below, floor of the solid side above: **slab
//!   edge** (R4), a slab seen from a taller neighbour, owned by the span above
//!   the slab;
//! * the open interval closed on both ends by its own floor and ceiling, with
//!   the other side solid everywhere: **closure** (R3), owned by the open side.
//!
//! For two single-span columns this is exactly R1 to R3 of [`super::derive`],
//! which is why the old code still answers those edges. This module only runs
//! where a side has several spans.
//!
//! A strip is cut at every crossing of two bounds inside the edge, so each
//! piece is a convex quad or triangle with one owner, the way the bow tie is
//! cut today. Heights are rounded to the grid unit; a piece may differ from
//! its neighbour by one unit at a crossing.

use crate::ResourceId;

use super::derive::{edge_key, open_top, quad_heights_len, DerivedKind, DerivedWall};
use super::edit::walls::{edge_corner_indices, edge_triangle};
use super::overrides::EdgeKey;
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::surface::GridSurface;
use super::units::SECTOR_UNITS;

/// Where a bound of an open interval comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rim {
    /// The floor of span `i`.
    Floor(usize),
    /// The ceiling of span `i`.
    Ceiling(usize),
}

/// One bound along the edge, in the canonical direction (`u = 0` at the
/// canonical start, `u = SECTOR_UNITS` at its end).
#[derive(Clone, Copy, Debug)]
struct Bound {
    h: [i32; 2],
    rim: Rim,
}

/// The open interval of one span along an edge. `None` is unbounded: a pit
/// below, open sky above, or a void triangle on the edge (no surface there).
#[derive(Clone, Copy, Debug)]
struct Open {
    span: usize,
    low: Option<Bound>,
    high: Option<Bound>,
}

/// A solid gap between two open intervals (or below the lowest, above the
/// highest, or the whole height of a column with no sector). `None` is
/// unbounded.
#[derive(Clone, Copy, Debug)]
struct Gap {
    low: Option<Bound>,
    high: Option<Bound>,
}

fn forward(edge: WallEdge) -> bool {
    matches!(edge, WallEdge::North | WallEdge::East)
}

fn bound(surface: &GridSurface, edge: WallEdge, rim: Rim) -> Option<Bound> {
    let [a, b] = edge_corner_indices(edge);
    let tri = edge_triangle(surface, edge);
    if surface.is_void(tri) {
        return None;
    }
    let heights = surface.heights_for(tri);
    let h = if forward(edge) {
        [heights[a], heights[b]]
    } else {
        [heights[b], heights[a]]
    };
    Some(Bound { h, rim })
}

fn open_intervals(sector: Option<&GridSector>, edge: WallEdge) -> Vec<Open> {
    let Some(sector) = sector else {
        return Vec::new();
    };
    sector
        .spans()
        .iter()
        .enumerate()
        .map(|(span, s)| Open {
            span,
            low: s
                .floor
                .as_ref()
                .and_then(|f| bound(f, edge, Rim::Floor(span))),
            high: s
                .ceiling
                .as_ref()
                .and_then(|c| bound(c, edge, Rim::Ceiling(span))),
        })
        .collect()
}

/// The solid gaps of a column whose open intervals are `open` (bottom to
/// top). A column with no sector is solid everywhere.
fn gaps(open: &[Open]) -> Vec<Gap> {
    if open.is_empty() {
        return vec![Gap {
            low: None,
            high: None,
        }];
    }
    let mut out = Vec::new();
    if let Some(low) = open[0].low {
        out.push(Gap {
            low: None,
            high: Some(low),
        });
    }
    for pair in open.windows(2) {
        if let (Some(low), Some(high)) = (pair[0].high, pair[1].low) {
            out.push(Gap {
                low: Some(low),
                high: Some(high),
            });
        }
    }
    if let Some(high) = open.last().and_then(|o| o.high) {
        out.push(Gap {
            low: Some(high),
            high: None,
        });
    }
    out
}

/// A bound's height at canonical `u`, rounded to the grid unit.
fn at(h: [i32; 2], u: i32) -> i32 {
    let len = i64::from(SECTOR_UNITS);
    let num = i64::from(h[1] - h[0]) * i64::from(u);
    let rounded = (2 * num + len).div_euclid(2 * len);
    h[0] + rounded as i32
}

/// The `u` (rounded) where two linear bounds are equal, if strictly inside
/// the edge.
fn crossing(a: [i32; 2], b: [i32; 2]) -> Option<i32> {
    let len = i64::from(SECTOR_UNITS);
    let mut num = i64::from(b[0] - a[0]) * len;
    let mut den = i64::from(a[1] - a[0]) - i64::from(b[1] - b[0]);
    if den == 0 {
        return None;
    }
    if den < 0 {
        num = -num;
        den = -den;
    }
    let u = (2 * num + den).div_euclid(2 * den);
    (0 < u && u < len).then_some(u as i32)
}

/// Remove consecutive repeats, including the wrap from the last point to
/// the first.
fn dedup_ring(mut ring: Vec<[i32; 2]>) -> Vec<[i32; 2]> {
    ring.dedup();
    while ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }
    ring
}

/// What closes a piece: its kind, and the span whose surface owns it on the
/// solid side (for a closure, the open side's own span).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Closes {
    Riser { solid_span: usize },
    Drop { solid_span: usize },
    Slab { solid_span: usize },
    Closure,
}

fn classify(low: Rim, high: Rim) -> Closes {
    match (low, high) {
        (Rim::Floor(_), Rim::Floor(k)) => Closes::Riser { solid_span: k },
        (Rim::Ceiling(j), Rim::Ceiling(_)) => Closes::Drop { solid_span: j },
        // A slab seen from a taller neighbour: the span above it owns the edge.
        (Rim::Ceiling(_), Rim::Floor(k)) => Closes::Slab { solid_span: k },
        (Rim::Floor(_), Rim::Ceiling(_)) => Closes::Closure,
    }
}

/// One piece of a strip: a ring in canonical `[u, y]` and what closes it.
struct Piece {
    ring: Vec<[i32; 2]>,
    closes: Closes,
}

/// The pieces of `open` minus the solid `gap`. Each piece lies between one
/// lower and one upper bound that are the active ones over its `u` range.
fn strip(open: Open, gap: Gap) -> Vec<Piece> {
    let lows: Vec<Bound> = [open.low, gap.low].into_iter().flatten().collect();
    let highs: Vec<Bound> = [open.high, gap.high].into_iter().flatten().collect();
    if lows.is_empty() || highs.is_empty() {
        return Vec::new();
    }
    let lines: Vec<[i32; 2]> = lows.iter().chain(highs.iter()).map(|b| b.h).collect();
    let mut cuts = vec![0, SECTOR_UNITS];
    for (i, a) in lines.iter().enumerate() {
        for b in &lines[i + 1..] {
            cuts.extend(crossing(*a, *b));
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let (u0, u1) = (pair[0], pair[1]);
        let mid = (u0 + u1) / 2;
        let low = lows.iter().copied().reduce(|best, b| {
            if at(b.h, mid) > at(best.h, mid) {
                b
            } else {
                best
            }
        });
        let high = highs.iter().copied().reduce(|best, b| {
            if at(b.h, mid) < at(best.h, mid) {
                b
            } else {
                best
            }
        });
        let (Some(low), Some(high)) = (low, high) else {
            continue;
        };
        let (b0, b1) = (at(low.h, u0), at(low.h, u1));
        let (t0, t1) = (at(high.h, u0), at(high.h, u1));
        if t0 <= b0 && t1 <= b1 {
            continue;
        }
        let ring = dedup_ring(vec![[u0, b0], [u1, b1], [u1, t1], [u0, t0]]);
        if ring.len() < 3 {
            continue;
        }
        out.push(Piece {
            ring,
            closes: classify(low.rim, high.rim),
        });
    }
    out
}

/// The material of the triangle of `surface` that holds `edge`.
fn material_on(surface: Option<&GridSurface>, edge: WallEdge) -> Option<ResourceId> {
    let surface = surface?;
    surface.tri(edge_triangle(surface, edge)).material
}

/// The floor or ceiling of span `index` of a column.
fn surface_of(sector: Option<&GridSector>, rim: Rim) -> Option<&GridSurface> {
    let span = sector?.spans().get(match rim {
        Rim::Floor(i) | Rim::Ceiling(i) => i,
    })?;
    match rim {
        Rim::Floor(_) => span.floor.as_ref(),
        Rim::Ceiling(_) => span.ceiling.as_ref(),
    }
}

/// The wall material of span `index` of a column, if any.
fn wall_of(sector: Option<&GridSector>, index: usize) -> Option<ResourceId> {
    sector?.spans().get(index)?.wall_material
}

/// The two columns of one physical edge and what they say.
struct Pair<'a> {
    room: &'a GridRoom,
    lo_cell: [i32; 2],
    lo_edge: WallEdge,
    lo: Option<&'a GridSector>,
    hi_cell: [i32; 2],
    hi_edge: WallEdge,
    hi: Option<&'a GridSector>,
    key: EdgeKey,
}

/// A face to emit: its kind, the host and owner cells and the host edge, the
/// canonical ring, and the material chain's first link that is not the
/// override.
struct Emit {
    kind: DerivedKind,
    host: [i32; 2],
    owner: [i32; 2],
    ring: Vec<[i32; 2]>,
    chained: Option<ResourceId>,
}

impl Pair<'_> {
    /// Every face seen from one side (the host) and owned by the other.
    fn faces_from(&self, host_is_lo: bool) -> Vec<DerivedWall> {
        let (host_cell, host_edge, host, owner_cell, owner_edge, owner) = if host_is_lo {
            (
                self.lo_cell,
                self.lo_edge,
                self.lo,
                self.hi_cell,
                self.hi_edge,
                self.hi,
            )
        } else {
            (
                self.hi_cell,
                self.hi_edge,
                self.hi,
                self.lo_cell,
                self.lo_edge,
                self.lo,
            )
        };
        let open_side = open_intervals(host, host_edge);
        let solid_side = gaps(&open_intervals(owner, owner_edge));
        let mut out = Vec::new();
        for interval in &open_side {
            for gap in &solid_side {
                for piece in strip(*interval, *gap) {
                    let emit = self.describe(
                        piece,
                        *interval,
                        (host_cell, host_edge, host),
                        (owner_cell, owner_edge, owner),
                    );
                    self.push(&mut out, host_edge, emit);
                }
                // Open sky over the open side and nothing at all on the other:
                // the closure runs up to the room's open wall height.
                if interval.high.is_none() && gap.low.is_none() && gap.high.is_none() {
                    if let (Some(low), true) = (interval.low, self.room.defaults.open_wall > 0) {
                        let top = open_top(self.room, self.key, low.h);
                        let ring = vec![
                            [0, low.h[0]],
                            [SECTOR_UNITS, low.h[1]],
                            [SECTOR_UNITS, top[1]],
                            [0, top[0]],
                        ];
                        let chained = wall_of(host, interval.span).or(material_on(
                            surface_of(host, Rim::Floor(interval.span)),
                            host_edge,
                        ));
                        let emit = Emit {
                            kind: DerivedKind::Closure,
                            host: host_cell,
                            owner: host_cell,
                            ring,
                            chained,
                        };
                        self.push(&mut out, host_edge, emit);
                    }
                }
            }
        }
        out
    }

    /// The face a piece makes: its kind and owner and the material chain.
    fn describe(
        &self,
        piece: Piece,
        interval: Open,
        host: ([i32; 2], WallEdge, Option<&GridSector>),
        owner: ([i32; 2], WallEdge, Option<&GridSector>),
    ) -> Emit {
        let (host_cell, _, host_sector) = host;
        let (owner_cell, owner_edge, owner_sector) = owner;
        let (kind, owner_cell, owner_index, owner_rim, surface_edge, surface_sector) =
            match piece.closes {
                Closes::Riser { solid_span } => (
                    DerivedKind::Riser,
                    owner_cell,
                    solid_span,
                    Rim::Floor(solid_span),
                    owner_edge,
                    owner_sector,
                ),
                Closes::Drop { solid_span } => (
                    DerivedKind::Drop,
                    owner_cell,
                    solid_span,
                    Rim::Ceiling(solid_span),
                    owner_edge,
                    owner_sector,
                ),
                Closes::Slab { solid_span } => (
                    DerivedKind::Slab,
                    owner_cell,
                    solid_span,
                    Rim::Floor(solid_span),
                    owner_edge,
                    owner_sector,
                ),
                Closes::Closure => (
                    DerivedKind::Closure,
                    host_cell,
                    interval.span,
                    Rim::Floor(interval.span),
                    host.1,
                    host_sector,
                ),
            };
        let surface = surface_of(surface_sector, owner_rim);
        // The chain reads the owner's wall material; a closure's owner is the
        // open side, whose span is `interval.span`.
        let chain_sector = match piece.closes {
            Closes::Closure => host_sector,
            _ => owner_sector,
        };
        let chained = wall_of(chain_sector, owner_index)
            .or(wall_of(host_sector, interval.span))
            .or(self.room.defaults.wall_material)
            .or(material_on(surface, surface_edge));
        Emit {
            kind,
            host: host_cell,
            owner: owner_cell,
            ring: piece.ring,
            chained,
        }
    }

    /// Turn a canonical ring into a [`DerivedWall`] hosted on `host_edge`.
    fn push(&self, out: &mut Vec<DerivedWall>, host_edge: WallEdge, emit: Emit) {
        let flip = !forward(host_edge);
        let ring: Vec<[i32; 2]> = emit
            .ring
            .iter()
            .map(|p| [if flip { SECTOR_UNITS - p[0] } else { p[0] }, p[1]])
            .collect();
        let at_end = |u: i32| ring.iter().filter(|p| p[0] == u).count();
        let heights = (ring.len() == 4 && at_end(0) == 2 && at_end(SECTOR_UNITS) == 2)
            .then(|| quad_heights_len(&ring, SECTOR_UNITS));
        let ovr = self.room.edge_overrides.get(&self.key);
        out.push(DerivedWall {
            kind: emit.kind,
            host: emit.host,
            edge: host_edge,
            back: false,
            owner: emit.owner,
            key: self.key,
            ring,
            heights,
            material: ovr.and_then(|o| o.material).or(emit.chained),
            hidden: ovr.is_some_and(|o| o.hidden),
            uv: ovr.and_then(|o| o.uv),
            solid: ovr.and_then(|o| o.solid),
            band_set: ovr.and_then(|o| o.band_set),
        });
    }
}

/// Every derived wall on the physical edge between `lo_cell`'s `lo_edge`
/// and `hi_cell`'s `hi_edge`, both directions, by the open-set rule.
pub(super) fn edge_walls(
    room: &GridRoom,
    lo_cell: [i32; 2],
    lo_edge: WallEdge,
    hi_cell: [i32; 2],
    hi_edge: WallEdge,
) -> Vec<DerivedWall> {
    let pair = Pair {
        room,
        lo_cell,
        lo_edge,
        lo: super::derive::sector_at(room, lo_cell),
        hi_cell,
        hi_edge,
        hi: super::derive::sector_at(room, hi_cell),
        key: edge_key(hi_cell, hi_edge),
    };
    let mut out = pair.faces_from(true);
    out.extend(pair.faces_from(false));
    out
}
