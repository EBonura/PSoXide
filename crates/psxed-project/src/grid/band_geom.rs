//! The polygons of a banded wall.
//!
//! A wall face is a region between a bottom rail and a top rail over an
//! interval of positions `u` along its edge, in grid units (`u` along the
//! walking direction, `y` the room relative height). Cut lines are more
//! rails. Every rail is linear in `u`, so the pieces are quads and
//! triangles, and where two rails cross inside the interval the pieces are
//! cut there so each stays planar and convex.
//!
//! Lines never cross: the cut `e_i(u)` is the line clamped between the
//! previous cut and the top rail, and every cut is then clamped into the
//! wall's own region, so a band whose strip collapses has zero height at
//! that `u` instead of overlapping its neighbour. Neighbouring bands share
//! their boundary exactly, so the pieces tile the wall without a gap or an
//! overlap (the property the tests check by area).
//!
//! A wall next to another wall on the same line shares its vertical end edge.
//! Where the two cut the edge at different heights (different band sets,
//! different heights) a vertex of one lies inside an edge of the other and a
//! one pixel crack can open there on the GPU. [`stitch`] inserts the
//! neighbour's heights into the edge and fans the polygon into triangles.

use super::bands::{Anchor, BandSet};
use super::surface::GridWall;

/// The region of a wall: `u` from `u[0]` to `u[1]`, the bottom and top rails
/// given at both ends. A triangle has its two rails meet at one end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Start and end position along the edge.
    pub u: [i32; 2],
    /// Bottom rail height at the start and the end.
    pub bottom: [i32; 2],
    /// Top rail height at the start and the end.
    pub top: [i32; 2],
}

impl Span {
    /// A span from a wall record's `[bottom a, bottom b, top b, top a]`
    /// heights over `u` from 0 to `length`.
    pub fn from_heights(h: [i32; 4], length: i32) -> Self {
        Self {
            u: [0, length],
            bottom: [h[0], h[1]],
            top: [h[3], h[2]],
        }
    }

    /// The span a ring of three or four corners `[u, y]` bounds, when its
    /// ends are vertical (a quad) or it is a triangle with a vertical edge.
    pub fn from_ring(ring: &[[i32; 2]]) -> Option<Self> {
        let lo = ring.iter().map(|p| p[0]).min()?;
        let hi = ring.iter().map(|p| p[0]).max()?;
        if lo == hi {
            return None;
        }
        let range_at = |u: i32| {
            let ys: Vec<i32> = ring.iter().filter(|p| p[0] == u).map(|p| p[1]).collect();
            (!ys.is_empty()).then(|| (*ys.iter().min().unwrap(), *ys.iter().max().unwrap()))
        };
        let (b0, t0) = range_at(lo)?;
        let (b1, t1) = range_at(hi)?;
        Some(Self {
            u: [lo, hi],
            bottom: [b0, b1],
            top: [t0, t1],
        })
    }
}

/// One strip of the wall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    /// The band it belongs to.
    pub band: usize,
    /// Corners `[u, y]` in the order `(u0, low)`, `(u1, low)`, `(u1, high)`,
    /// `(u0, high)`, repeated corners dropped, so three or four corners
    /// before [`stitch`].
    pub ring: Vec<[i32; 2]>,
}

/// Anchor extents of a stack of authored walls on one edge: the lowest
/// bottom and the highest top over the stack, across `length`, so one trim
/// or frieze runs along the whole stack and not along each segment.
pub fn stack_reference(walls: &[GridWall], length: i32) -> Option<Span> {
    let live: Vec<&GridWall> = walls.iter().filter(|w| !w.is_degenerate()).collect();
    if live.is_empty() {
        return None;
    }
    let lo = |i: usize| live.iter().map(|w| w.heights[i]).min().unwrap_or(0);
    let hi = |i: usize| live.iter().map(|w| w.heights[i]).max().unwrap_or(0);
    Some(Span {
        u: [0, length],
        bottom: [lo(0), lo(1)],
        top: [hi(3), hi(2)],
    })
}

/// A rail: its height at the start and the end of the span.
#[derive(Clone, Copy)]
struct Rail([f64; 2]);

impl Rail {
    fn at(&self, span: &Span, u: i32) -> f64 {
        let t = f64::from(u - span.u[0]) / f64::from(span.u[1] - span.u[0]);
        self.0[0] + (self.0[1] - self.0[0]) * t
    }
}

fn rail_of(anchor: Anchor, reference: &Span) -> Rail {
    match anchor {
        Anchor::Bottom(o) => Rail(reference.bottom.map(|y| f64::from(y + o))),
        Anchor::Top(o) => Rail(reference.top.map(|y| f64::from(y - o))),
        Anchor::Abs(y) => Rail([f64::from(y); 2]),
    }
}

/// Positions strictly inside the span where two of `rails` cross, rounded to
/// whole units and deduplicated.
fn crossings(rails: &[Rail], span: &Span) -> Vec<i32> {
    let mut out = Vec::new();
    for (i, a) in rails.iter().enumerate() {
        for b in &rails[i + 1..] {
            let (d0, d1) = (a.0[0] - b.0[0], a.0[1] - b.0[1]);
            if d0 * d1 < 0.0 {
                let t = d0 / (d0 - d1);
                let u = f64::from(span.u[0]) + f64::from(span.u[1] - span.u[0]) * t;
                let u = u.round() as i32;
                if u > span.u[0] && u < span.u[1] {
                    out.push(u);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The rails of one wall: its own, the reference extent's and one per cut
/// line.
struct Rails {
    lines: Vec<Rail>,
    bottom: Rail,
    top: Rail,
    ref_bottom: Rail,
    ref_top: Rail,
}

impl Rails {
    fn new(span: &Span, reference: &Span, set: &BandSet) -> Self {
        Self {
            lines: set.lines.iter().map(|a| rail_of(*a, reference)).collect(),
            bottom: Rail(span.bottom.map(f64::from)),
            top: Rail(span.top.map(f64::from)),
            ref_bottom: Rail(reference.bottom.map(f64::from)),
            ref_top: Rail(reference.top.map(f64::from)),
        }
    }

    /// The boundary heights at `u`, from the bottom rail (boundary 0) up
    /// through every cut to the top rail (boundary `n + 1`), rounded.
    fn boundary(&self, span: &Span, u: i32) -> Vec<i32> {
        let (lo, hi) = (self.bottom.at(span, u), self.top.at(span, u));
        let (ref_bottom, ref_top) = (self.ref_bottom.at(span, u), self.ref_top.at(span, u));
        let hi = hi.max(lo);
        let mut cuts = vec![lo];
        let mut previous = ref_bottom;
        for line in &self.lines {
            // Never below the cut before it, never above the reference top.
            let cut = line.at(span, u).max(previous).min(ref_top);
            previous = cut;
            cuts.push(cut.clamp(lo, hi));
        }
        cuts.push(hi);
        cuts.into_iter().map(|y| y.round() as i32).collect()
    }
}

/// A wall rail's height at position `u`, rounded.
pub fn rail_height(bottom_or_top: [i32; 2], span: &Span, u: i32) -> i32 {
    Rail(bottom_or_top.map(f64::from)).at(span, u).round() as i32
}

/// Where `anchor` falls at both ends of `span` (the same value the pieces
/// are cut at), clamped into the wall.
pub fn anchor_ends(span: &Span, reference: &Span, anchor: Anchor) -> [i32; 2] {
    let rail = rail_of(anchor, reference);
    [span.u[0], span.u[1]].map(|u| {
        let lo = Rail(span.bottom.map(f64::from)).at(span, u);
        let hi = Rail(span.top.map(f64::from)).at(span, u).max(lo);
        rail.at(span, u).clamp(lo, hi).round() as i32
    })
}

/// The heights of the cut lines (not the wall's own rails) at position `u`,
/// bottom up, as the pieces draw them.
pub fn cuts_at(span: &Span, reference: &Span, set: &BandSet, u: i32) -> Vec<i32> {
    let mut all = Rails::new(span, reference, set).boundary(span, u);
    all.pop();
    all.remove(0);
    all
}

/// Cut `span` with `set`. `reference` is the extent the anchors are measured
/// against (the wall's own span, or the whole edge's for a stack of
/// authored segments, so one trim runs at the base of the stack).
pub fn bands(span: &Span, reference: &Span, set: &BandSet) -> Vec<Piece> {
    let rails = Rails::new(span, reference, set);
    let mut all = rails.lines.clone();
    all.extend([rails.bottom, rails.top, rails.ref_bottom, rails.ref_top]);
    let mut grid = vec![span.u[0]];
    grid.extend(crossings(&all, span));
    grid.push(span.u[1]);
    let boundary = |u: i32| rails.boundary(span, u);
    let lines = &rails.lines;
    let columns: Vec<(i32, Vec<i32>)> = grid.iter().map(|&u| (u, boundary(u))).collect();
    let mut pieces = Vec::new();
    for band in 0..=lines.len() {
        for pair in columns.windows(2) {
            let ((ua, a), (ub, b)) = (&pair[0], &pair[1]);
            let corners = [
                [*ua, a[band]],
                [*ub, b[band]],
                [*ub, b[band + 1]],
                [*ua, a[band + 1]],
            ];
            let mut ring: Vec<[i32; 2]> = Vec::with_capacity(4);
            for corner in corners {
                if ring.last() != Some(&corner) {
                    ring.push(corner);
                }
            }
            while ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            if ring.len() >= 3 && twice_area(&ring) != 0 {
                pieces.push(Piece { band, ring });
            }
        }
    }
    pieces
}

/// Twice the signed area of a ring.
pub fn twice_area(ring: &[[i32; 2]]) -> i64 {
    (0..ring.len())
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            i64::from(a[0]) * i64::from(b[1]) - i64::from(b[0]) * i64::from(a[1])
        })
        .sum()
}

/// The heights at which `pieces` have a vertex on the vertical line at `u`.
pub fn heights_at(pieces: &[Piece], u: i32) -> Vec<i32> {
    let mut out: Vec<i32> = pieces
        .iter()
        .flat_map(|p| p.ring.iter().filter(|c| c[0] == u).map(|c| c[1]))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Insert into every piece's vertical edge at `u` the heights in `extra`
/// that lie strictly inside it. `extra` is what the wall across that edge
/// cuts it at.
pub fn stitch_edge(pieces: &mut [Piece], u: i32, extra: &[i32]) {
    for piece in pieces {
        let on: Vec<usize> = (0..piece.ring.len())
            .filter(|&i| piece.ring[i][0] == u)
            .collect();
        if on.len() != 2 {
            continue;
        }
        let (i, j) = (on[0], on[1]);
        // The two corners are consecutive on the ring (wrapping).
        let n = piece.ring.len();
        let (first, second) = if (i + 1) % n == j {
            (i, j)
        } else if (j + 1) % n == i {
            (j, i)
        } else {
            continue;
        };
        let (a, b) = (piece.ring[first][1], piece.ring[second][1]);
        let (lo, hi) = (a.min(b), a.max(b));
        let mut inside: Vec<i32> = extra
            .iter()
            .copied()
            .filter(|&h| h > lo && h < hi)
            .collect();
        if inside.is_empty() {
            continue;
        }
        // Walk from `a` to `b`.
        inside.sort_unstable();
        if a > b {
            inside.reverse();
        }
        let at = first + 1;
        for (k, h) in inside.into_iter().enumerate() {
            piece.ring.insert(at + k, [u, h]);
        }
    }
}

/// Split a convex ring of any size into rings of at most four corners
/// (triangles where it has more), dropping the zero-area slivers collinear
/// corners make.
pub fn fan(ring: &[[i32; 2]]) -> Vec<Vec<[i32; 2]>> {
    if ring.len() <= 4 {
        return vec![ring.to_vec()];
    }
    let sign = twice_area(ring).signum();
    let mut rest: Vec<[i32; 2]> = ring.to_vec();
    let mut out = Vec::new();
    while rest.len() > 4 {
        let n = rest.len();
        // The ear to cut off: a corner whose triangle has the polygon's own
        // orientation (a collinear corner has none).
        let ear = (0..n).find(|&i| {
            let tri = [rest[(i + n - 1) % n], rest[i], rest[(i + 1) % n]];
            twice_area(&tri).signum() == sign
        });
        let Some(i) = ear else { break };
        let tri = vec![rest[(i + n - 1) % n], rest[i], rest[(i + 1) % n]];
        out.push(tri);
        rest.remove(i);
    }
    // Two corners of an edge that was collinear may remain adjacent to a
    // zero-area triangle only if the loop broke early; drop slivers.
    out.push(rest);
    out.retain(|r| twice_area(r) != 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::bands::BandSet;

    fn set(lines: &[Anchor]) -> BandSet {
        let mut set = BandSet::single("s", None);
        for (i, line) in lines.iter().enumerate() {
            assert!(set.insert_line(i, *line));
        }
        set
    }

    /// The pieces' areas, by band.
    fn areas(pieces: &[Piece], n: usize) -> Vec<i64> {
        let mut out = vec![0i64; n];
        for p in pieces {
            out[p.band] += twice_area(&p.ring).abs();
        }
        out
    }

    fn face_area(span: &Span) -> i64 {
        let du = i64::from(span.u[1] - span.u[0]);
        let h0 = i64::from(span.top[0] - span.bottom[0]);
        let h1 = i64::from(span.top[1] - span.bottom[1]);
        du * (h0 + h1)
    }

    #[test]
    fn a_rectangle_cut_by_a_trim_and_a_frieze_makes_three_quads() {
        let span = Span::from_heights([0, 0, 2048, 2048], 1024);
        let pieces = bands(&span, &span, &set(&[Anchor::Bottom(256), Anchor::Top(256)]));
        assert_eq!(pieces.len(), 3);
        assert!(pieces.iter().all(|p| p.ring.len() == 4));
        // Trim 0..256, body 256..1792, frieze 1792..2048.
        let heights: Vec<(i32, i32)> = pieces
            .iter()
            .map(|p| {
                (
                    p.ring.iter().map(|c| c[1]).min().unwrap(),
                    p.ring.iter().map(|c| c[1]).max().unwrap(),
                )
            })
            .collect();
        assert_eq!(heights, vec![(0, 256), (256, 1792), (1792, 2048)]);
        assert_eq!(areas(&pieces, 3).iter().sum::<i64>(), face_area(&span));
    }

    #[test]
    fn a_bottom_anchored_line_follows_a_sloped_floor_and_a_top_one_a_sloped_ceiling() {
        // Floor rises 0 to 512 along the wall, ceiling stays at 2048.
        let span = Span::from_heights([0, 512, 2048, 2048], 1024);
        let pieces = bands(&span, &span, &set(&[Anchor::Bottom(256), Anchor::Top(256)]));
        assert_eq!(pieces.len(), 3);
        let trim = &pieces[0].ring;
        // The trim is a parallelogram: 256 tall at both ends.
        let at = |u: i32| -> Vec<i32> { trim.iter().filter(|c| c[0] == u).map(|c| c[1]).collect() };
        assert_eq!((at(0), at(1024)), (vec![0, 256], vec![512, 768]));
        // The frieze hangs 256 under the flat ceiling at both ends.
        let frieze = &pieces[2].ring;
        assert!(frieze.iter().all(|c| c[1] == 1792 || c[1] == 2048));
        assert_eq!(areas(&pieces, 3).iter().sum::<i64>(), face_area(&span));
    }

    #[test]
    fn an_absolute_line_across_a_slope_breaks_into_a_triangle_and_a_trapezoid() {
        // The floor rises 0 to 1024; a datum at 512 is crossed half way.
        let span = Span::from_heights([0, 1024, 2048, 2048], 1024);
        let pieces = bands(&span, &span, &set(&[Anchor::Abs(512)]));
        let below: Vec<&Piece> = pieces.iter().filter(|p| p.band == 0).collect();
        let above: Vec<&Piece> = pieces.iter().filter(|p| p.band == 1).collect();
        // Below the datum: a triangle up to the crossing, then nothing (the
        // floor is above the line beyond it).
        assert_eq!(below.len(), 1);
        assert_eq!(below[0].ring.len(), 3);
        assert!(below[0].ring.iter().any(|c| c[0] == 512 && c[1] == 512));
        // Above: a quad before the crossing is cut at it, a trapezoid after.
        assert_eq!(above.len(), 2);
        assert_eq!(areas(&pieces, 2).iter().sum::<i64>(), face_area(&span));
    }

    #[test]
    fn lines_never_cross_and_a_collapsed_strip_has_no_height() {
        // A line above the top rail and one below the bottom: both clamp.
        let span = Span::from_heights([256, 256, 768, 768], 1024);
        let pieces = bands(&span, &span, &set(&[Anchor::Abs(0), Anchor::Abs(4096)]));
        // Only the middle band has area.
        assert!(pieces.iter().all(|p| p.band == 1), "{pieces:?}");
        assert_eq!(areas(&pieces, 3), vec![0, face_area(&span), 0]);
        // Two lines out of order are clamped, not crossed.
        let pieces = bands(&span, &span, &set(&[Anchor::Abs(600), Anchor::Abs(400)]));
        assert_eq!(areas(&pieces, 3).iter().sum::<i64>(), face_area(&span));
        for p in &pieces {
            assert!(twice_area(&p.ring) > 0 || twice_area(&p.ring) < 0);
        }
    }

    #[test]
    fn a_triangular_wall_is_cut_into_triangles_and_quads_that_tile_it() {
        // Two rails meet at the far end.
        let span = Span {
            u: [0, 1024],
            bottom: [0, 1024],
            top: [1024, 1024],
        };
        let pieces = bands(&span, &span, &set(&[Anchor::Bottom(128), Anchor::Top(128)]));
        assert_eq!(areas(&pieces, 3).iter().sum::<i64>(), face_area(&span));
        assert!(pieces.iter().all(|p| (3..=4).contains(&p.ring.len())));
    }

    #[test]
    fn pieces_tile_random_walls_without_gap_or_overlap() {
        let mut state = 0x1234_5678u32;
        let mut next = |m: u32| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) % m
        };
        for case in 0..400 {
            let b = [next(1024) as i32, next(1024) as i32];
            let t = [b[0] + next(2048) as i32, b[1] + next(2048) as i32];
            let span = Span {
                u: [0, 1024],
                bottom: b,
                top: t,
            };
            let mut lines = Vec::new();
            for _ in 0..(1 + next(4)) {
                let v = next(2560) as i32 - 256;
                lines.push(match next(3) {
                    0 => Anchor::Bottom(v),
                    1 => Anchor::Top(v),
                    _ => Anchor::Abs(v),
                });
            }
            let s = set(&lines);
            let pieces = bands(&span, &span, &s);
            let total: i64 = pieces.iter().map(|p| twice_area(&p.ring).abs()).sum();
            // Rounding to whole units may move a crossing by half a unit.
            let slack = 4 * 1024;
            assert!(
                (total - face_area(&span)).abs() <= slack,
                "case {case}: {total} against {} for {span:?} {lines:?}",
                face_area(&span)
            );
            for p in &pieces {
                assert!((3..=4).contains(&p.ring.len()), "{p:?}");
            }
        }
    }

    #[test]
    fn stitching_inserts_the_neighbours_cut_heights_and_fans_the_result() {
        let span = Span::from_heights([0, 0, 2048, 2048], 1024);
        let mut pieces = bands(&span, &span, &set(&[]));
        assert_eq!(pieces.len(), 1);
        // The wall across the end edge cuts it at 256 and 1792.
        stitch_edge(&mut pieces, 1024, &[0, 256, 1792, 2048]);
        assert_eq!(pieces[0].ring.len(), 6);
        assert_eq!(heights_at(&pieces, 1024), vec![0, 256, 1792, 2048]);
        let fanned = fan(&pieces[0].ring);
        assert!(fanned.iter().all(|r| r.len() <= 4));
        // The fan covers the same area.
        let total: i64 = fanned.iter().map(|r| twice_area(r).abs()).sum();
        assert_eq!(total, twice_area(&pieces[0].ring).abs());
        // Heights outside the edge are ignored.
        let mut again = bands(&span, &span, &set(&[]));
        stitch_edge(&mut again, 1024, &[-5, 2048, 9999]);
        assert_eq!(again[0].ring.len(), 4);
    }

    #[test]
    fn from_ring_reads_quads_and_triangles_back() {
        let quad = vec![[0, 0], [1024, 128], [1024, 900], [0, 700]];
        let s = Span::from_ring(&quad).unwrap();
        assert_eq!((s.bottom, s.top), ([0, 128], [700, 900]));
        // A triangle: the apex end has both rails at one height.
        let tri = vec![[0, 0], [0, 512], [300, 200]];
        let s = Span::from_ring(&tri).unwrap();
        assert_eq!((s.u, s.bottom, s.top), ([0, 300], [0, 200], [512, 200]));
        assert!(Span::from_ring(&[[5, 5]]).is_none());
    }
}
